use evotengine::provider::stream::sse::SseReader;
use futures::stream;
use tokio_util::sync::CancellationToken;

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[tokio::test]
async fn every_byte_boundary_preserves_utf8_crlf_multiline_and_eof() -> TestResult {
    let bytes =
        ": ping\r\n\r\nevent: delta\r\ndata: 中文🙂\r\ndata: second\r\n\r\ndata: final".as_bytes();
    for split in 0..=bytes.len() {
        let chunks = vec![Ok(bytes[..split].to_vec()), Ok(bytes[split..].to_vec())];
        let mut reader = SseReader::from_stream(stream::iter(chunks));
        let cancel = CancellationToken::new();
        let first = reader.next(&cancel).await?.ok_or("missing delta")?;
        assert_eq!(first.event, "delta");
        assert_eq!(first.data, "中文🙂\nsecond");
        let last = reader.next(&cancel).await?.ok_or("missing final")?;
        assert_eq!(last.data, "final");
        assert!(reader.next(&cancel).await?.is_none());
        assert!(reader.next(&cancel).await?.is_none());
    }
    Ok(())
}

#[tokio::test]
async fn reads_only_as_demanded_and_drop_releases_upstream() -> TestResult {
    use std::sync::atomic::AtomicUsize;
    use std::sync::atomic::Ordering;
    use std::sync::Arc;

    use futures::StreamExt;
    let polls = Arc::new(AtomicUsize::new(0));
    let held = Arc::new(());
    let weak = Arc::downgrade(&held);
    let counter = polls.clone();
    let source = stream::repeat_with(move || {
        let _ = &held;
        counter.fetch_add(1, Ordering::SeqCst);
        Ok(b"data: next\n\n".to_vec())
    })
    .boxed();
    let mut reader = SseReader::from_stream(source);
    assert_eq!(polls.load(Ordering::SeqCst), 0);
    assert!(reader.next(&CancellationToken::new()).await?.is_some());
    assert_eq!(polls.load(Ordering::SeqCst), 1);
    drop(reader);
    assert!(weak.upgrade().is_none());
    Ok(())
}

#[tokio::test]
async fn cancellation_interrupts_idle_reads_and_partial_frame_errors_are_visible() -> TestResult {
    let mut reader = SseReader::from_stream(stream::pending());
    let cancel = CancellationToken::new();
    let trigger = cancel.clone();
    let task = tokio::spawn(async move { reader.next(&cancel).await });
    trigger.cancel();
    let result = tokio::time::timeout(std::time::Duration::from_secs(1), task).await??;
    assert_eq!(result.err().as_deref(), Some("cancelled"));
    let mut reader = SseReader::from_stream(stream::iter(vec![
        Ok(b"data: partial".to_vec()),
        Err("disconnected".into()),
    ]));
    assert_eq!(
        reader
            .next(&CancellationToken::new())
            .await
            .err()
            .as_deref(),
        Some("disconnected")
    );
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn silent_or_partial_connections_fail_within_one_minute() {
    use std::time::Duration;

    use futures::StreamExt;
    for prefix in [b"".as_slice(), b"data: partial", b": heartbeat\n\n"] {
        let source = stream::iter(vec![Ok(prefix.to_vec())]).chain(stream::pending());
        let mut reader = SseReader::from_stream(source);
        let start = tokio::time::Instant::now();
        let error = reader.next(&CancellationToken::new()).await.err();
        assert_eq!(
            error.as_deref(),
            Some("Stream read timed out after 60s without data")
        );
        assert_eq!(start.elapsed(), Duration::from_secs(60));
    }
}

#[tokio::test(start_paused = true)]
async fn heartbeats_and_slow_output_reset_read_deadlines_without_a_total_time_limit() -> TestResult
{
    use std::time::Duration;

    let source = stream::unfold(0, |index| async move {
        if index > 40 {
            return None;
        }
        tokio::time::sleep(Duration::from_secs(59)).await;
        let bytes = if index < 40 {
            b": heartbeat\n\n".to_vec()
        } else {
            b"data: recovered\n\n".to_vec()
        };
        Some((Ok(bytes), index + 1))
    });
    let mut reader = SseReader::from_stream(source);
    let start = tokio::time::Instant::now();
    let event = reader
        .next(&CancellationToken::new())
        .await?
        .ok_or("missing event")?;
    assert_eq!(event.data, "recovered");
    assert!(start.elapsed() > Duration::from_secs(35 * 60));
    assert!(reader.next(&CancellationToken::new()).await?.is_none());
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn timed_out_readers_release_the_dead_connection_on_drop() {
    let held = std::sync::Arc::new(());
    let weak = std::sync::Arc::downgrade(&held);
    let source = stream::unfold(held, |held| async move {
        futures::future::pending::<()>().await;
        Some((Ok(Vec::new()), held))
    });
    let mut reader = SseReader::from_stream(source);
    assert!(reader.next(&CancellationToken::new()).await.is_err());
    drop(reader);
    assert!(weak.upgrade().is_none());
}

#[tokio::test]
async fn oversize_unterminated_frames_fail_instead_of_accumulating() {
    let mut reader =
        SseReader::from_stream(stream::iter(vec![Ok(vec![b'x'; 8 * 1024 * 1024 + 1])]));
    assert!(reader.next(&CancellationToken::new()).await.is_err());
}

#[tokio::test]
async fn terminal_frame_can_be_consumed_without_waiting_for_socket_eof() -> TestResult {
    use futures::StreamExt;
    let stream = stream::iter(vec![Ok(b"data: [DONE]\n\n".to_vec())]).chain(stream::pending());
    let mut reader = SseReader::from_stream(stream);
    let event = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        reader.next(&CancellationToken::new()),
    )
    .await??;
    assert_eq!(event.ok_or("missing terminal")?.data, "[DONE]");
    drop(reader);
    Ok(())
}

#[tokio::test]
async fn exact_frame_limit_accepts_split_separator() -> TestResult {
    let mut frame = b"data: ".to_vec();
    frame.resize(8 * 1024 * 1024, b'x');
    for separator in [b"\n\n".as_slice(), b"\r\n\r\n".as_slice()] {
        let mut chunks = vec![Ok(frame.clone())];
        chunks.extend(separator.iter().map(|byte| Ok(vec![*byte])));
        let mut reader = SseReader::from_stream(stream::iter(chunks));
        assert!(reader.next(&CancellationToken::new()).await?.is_some());
    }
    Ok(())
}

#[test]
fn providers_use_pull_reader_instead_of_detached_parser_queues() {
    for source in [
        include_str!("../../src/provider/anthropic/sse_decode.rs"),
        include_str!("../../src/provider/openai_compat/sse_decode.rs"),
        include_str!("../../src/provider/openai_responses/sse_decode.rs"),
    ] {
        assert!(source.contains("SseReader::new(response)"));
        assert!(!source.contains("unbounded_channel"));
        assert!(!source.contains("tokio::spawn"));
    }
}
