//! Tests for shared stream HTTP helpers.

use evotengine::provider::stream::http::check_error_status;
use evotengine::provider::stream::http::classify_json_error;
use evotengine::provider::stream::http::extract_json_error_message;
use evotengine::provider::stream::http::StreamResponseKind;
use evotengine::provider::ProviderError;

/// Exercise the real shared client, not just the injected SSE transport. The
/// peer accepts the request but never sends headers (or stops after headers).
async fn silent_http_peer(send_headers: bool) -> Result<(), Box<dyn std::error::Error>> {
    use std::time::Duration;

    use tokio::io::AsyncReadExt;
    use tokio::io::AsyncWriteExt;

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let url = format!("http://{}", listener.local_addr()?);
    let (ready_tx, mut ready_rx) = tokio::sync::oneshot::channel();
    let peer = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await?;
        let mut request = Vec::new();
        let mut buffer = [0; 4096];
        while !request.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
            let count = socket.read(&mut buffer).await?;
            if count == 0 {
                return Err(std::io::Error::from(std::io::ErrorKind::UnexpectedEof));
            }
            request.extend_from_slice(&buffer[..count]);
        }
        if send_headers {
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\n")
                .await?;
        }
        let _ = ready_tx.send(());
        futures::future::pending::<()>().await;
        drop(socket);
        Ok::<(), std::io::Error>(())
    });
    let client = evotengine::provider::http_client()?;
    let request = evotengine::provider::stream::http::send_stream_request(client.get(url));
    tokio::pin!(request);
    let start;
    let result = if send_headers {
        let response = request.await?;
        let body = evotengine::provider::stream::http::read_text_body(response);
        tokio::pin!(body);
        assert!(futures::poll!(&mut body).is_pending());
        ready_rx.await?;
        tokio::time::pause();
        start = tokio::time::Instant::now();
        body.await
    } else {
        tokio::select! {
            ready = &mut ready_rx => ready?,
            result = &mut request => return Err(format!("request ended before peer stalled: {result:?}").into()),
        }
        tokio::time::pause();
        start = tokio::time::Instant::now();
        request.await.map(|_| String::new())
    };
    peer.abort();
    let _ = peer.await;
    let error = result.err().ok_or("silent peer unexpectedly succeeded")?;
    assert!(matches!(error, ProviderError::Network(_)));
    assert!(evotengine::retry::should_retry(&error));
    assert!(start.elapsed() <= Duration::from_secs(61));
    Ok(())
}

#[tokio::test]
async fn silent_response_headers_timeout_without_an_explicit_interrupt(
) -> Result<(), Box<dyn std::error::Error>> {
    silent_http_peer(false).await
}

#[tokio::test]
async fn silent_response_body_timeout_without_an_explicit_interrupt(
) -> Result<(), Box<dyn std::error::Error>> {
    silent_http_peer(true).await
}

#[tokio::test]
async fn an_upload_stall_is_bounded_and_retryable_instead_of_hanging(
) -> Result<(), Box<dyn std::error::Error>> {
    use std::time::Duration;
    use std::time::Instant;

    // A peer that accepts the connection, never reads a byte, and never
    // answers: the client blocks in the body write where `read_timeout` cannot
    // arm. This is the shape of a network drop during upload of a large
    // context.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let url = format!("http://{}", listener.local_addr()?);
    let peer = tokio::spawn(async move {
        let (socket, _) = listener.accept().await?;
        futures::future::pending::<()>().await;
        drop(socket);
        Ok::<(), std::io::Error>(())
    });
    let client = evotengine::provider::http_client()?;
    // Larger than any socket send buffer, so the write itself must block.
    let body = vec![b'x'; 32 * 1024 * 1024];
    let started = Instant::now();
    let error = evotengine::provider::stream::http::send_stream_request_with_deadline(
        client.post(url).body(body),
        Duration::from_secs(2),
    )
    .await
    .err()
    .ok_or("stalled upload unexpectedly succeeded")?;
    peer.abort();
    let _ = peer.await;
    assert!(matches!(error, ProviderError::Network(_)), "{error:?}");
    assert!(
        error
            .to_string()
            .contains("did not connect, upload, or return headers"),
        "{error}"
    );
    assert!(evotengine::retry::should_retry(&error));
    assert!(
        started.elapsed() < Duration::from_secs(15),
        "{:?}",
        started.elapsed()
    );
    Ok(())
}

#[test]
fn standard_model_errors_are_fatal_across_protocols_without_gateway_hints() {
    for value in [
        serde_json::json!({"error":{"type":"invalid_request_error","code":"model_not_found","message":"Not available"}}),
        serde_json::json!({"type":"error","error":{"type":"not_found_error","message":"Model not found."}}),
        serde_json::json!({"type":"response.failed","response":{"error":{"code":"model_not_found","message":"Not available"}}}),
    ] {
        let error = classify_json_error(&value);
        assert!(matches!(error, ProviderError::ModelNotFound(_)));
        assert!(!evotengine::retry::should_retry(&error));
    }
    let missing_route = classify_json_error(
        &serde_json::json!({"error":{"type":"not_found_error","message":"Route not found"}}),
    );
    assert!(!matches!(missing_route, ProviderError::ModelNotFound(_)));
    assert!(!evotengine::retry::should_retry(&missing_route));
}

// ---------------------------------------------------------------------------
// extract_json_error_message
// ---------------------------------------------------------------------------

#[test]
fn structured_stream_code_is_not_lost_behind_generic_type_or_message() {
    for (code, is_overflow) in [
        ("context_length_exceeded", true),
        ("overloaded_error", false),
    ] {
        let value = serde_json::json!({"error":{"type":"server_error","code":code,"message":"Request failed"}});
        let error = classify_json_error(&value);
        if is_overflow {
            assert!(matches!(error, ProviderError::ContextOverflow { .. }));
            assert!(!evotengine::retry::should_retry(&error));
        } else {
            assert!(matches!(error, ProviderError::Overloaded(_)));
        }
    }
}

#[test]
fn extract_anthropic_error_message() {
    let value = serde_json::json!({
        "type": "error",
        "error": {
            "type": "overloaded_error",
            "message": "Overloaded"
        }
    });
    let msg = extract_json_error_message(&value);
    assert_eq!(msg, Some("overloaded_error: Overloaded".into()));
}

#[test]
fn extract_openai_error_message() {
    let value = serde_json::json!({
        "error": {
            "message": "server error"
        }
    });
    let msg = extract_json_error_message(&value);
    assert_eq!(msg, Some("server error".into()));
}

#[test]
fn extract_openai_error_code_only() {
    let value = serde_json::json!({
        "error": {
            "code": "insufficient_quota",
            "message": "You exceeded your current quota"
        }
    });
    let msg = extract_json_error_message(&value);
    assert_eq!(
        msg,
        Some("insufficient_quota: You exceeded your current quota".into())
    );
}

#[test]
fn extract_generic_message_field() {
    let value = serde_json::json!({
        "message": "internal error"
    });
    let msg = extract_json_error_message(&value);
    assert_eq!(msg, Some("internal error".into()));
}

#[test]
fn extract_type_only() {
    let value = serde_json::json!({
        "type": "rate_limit_error"
    });
    let msg = extract_json_error_message(&value);
    assert_eq!(msg, Some("rate_limit_error".into()));
}

#[test]
fn extract_no_known_fields() {
    let value = serde_json::json!({"foo": "bar"});
    let msg = extract_json_error_message(&value);
    assert_eq!(msg, None);
}

#[test]
fn extract_openrouter_metadata_raw_json() {
    let value = serde_json::json!({
        "type": "error",
        "error": {
            "type": "invalid_request_error",
            "message": "Provider returned error"
        },
        "metadata": {
            "raw": "{\"error\":{\"type\":\"invalid_request_error\",\"message\":\"prompt is too long\"}}",
            "provider_name": "Stealth"
        }
    });
    let msg = extract_json_error_message(&value).expect("message");
    assert!(msg.starts_with("invalid_request_error: Provider returned error"));
    assert!(msg.contains("prompt is too long"));
    assert_eq!(msg.matches("prompt is too long").count(), 1);
}

#[test]
fn extract_openrouter_metadata_raw_plain_text() {
    let value = serde_json::json!({
        "error": {
            "type": "invalid_request_error",
            "message": "Provider returned error",
            "metadata": {"raw": "max_tokens too large"}
        }
    });
    let msg = extract_json_error_message(&value).expect("message");
    assert!(msg.starts_with("invalid_request_error: Provider returned error"));
    assert!(msg.contains("max_tokens too large"));
    assert_eq!(msg.matches("max_tokens too large").count(), 1);
}

#[test]
fn extract_does_not_duplicate_raw_already_in_the_message() {
    let value = serde_json::json!({
        "error": {
            "type": "invalid_request_error",
            "message": "Unsupported tool schema"
        },
        "metadata": {"raw": "Unsupported tool schema"}
    });
    let msg = extract_json_error_message(&value);
    assert_eq!(
        msg,
        Some("invalid_request_error: Unsupported tool schema".into())
    );
}

#[test]
fn extract_opaque_raw_is_still_appended() {
    let value = serde_json::json!({
        "error": {
            "type": "invalid_request_error",
            "message": "Provider returned error"
        },
        "metadata": {"raw": "ERROR", "provider_name": "Stealth"}
    });
    let msg = extract_json_error_message(&value).expect("message");
    assert!(msg.contains("Provider returned error"));
    assert!(msg.contains("ERROR"));
}

// ---------------------------------------------------------------------------
// classify_json_error
// ---------------------------------------------------------------------------

#[test]
fn classify_overflow_json() {
    let value = serde_json::json!({
        "error": {
            "type": "invalid_request_error",
            "message": "prompt is too long: 213462 tokens > 200000 maximum"
        }
    });
    let err = classify_json_error(&value);
    assert!(err.is_context_overflow());
    assert!(!evotengine::retry::should_retry(&err));
}

#[test]
fn classify_internal_server_error_json_is_retryable() {
    let value = serde_json::json!({
        "error": {
            "type": "api_error",
            "message": "Internal server error"
        }
    });
    let err = classify_json_error(&value);
    assert!(matches!(err, ProviderError::Transient { .. }));
    assert!(evotengine::retry::should_retry(&err));
}

#[test]
fn classify_overloaded_json_is_retryable() {
    let value = serde_json::json!({
        "type": "error",
        "error": {
            "type": "overloaded_error",
            "message": "service is overloaded"
        }
    });
    let err = classify_json_error(&value);
    assert!(matches!(err, ProviderError::Overloaded(_)));
    assert!(evotengine::retry::should_retry(&err));
}

#[test]
fn classify_no_message_uses_full_json() {
    // No recognizable error fields: the payload still arrived on an accepted
    // (2xx) request, so it defaults to a retryable transient error rather
    // than failing hard on unknown shapes.
    let value = serde_json::json!({"foo": "bar"});
    let err = classify_json_error(&value);
    assert!(matches!(err, ProviderError::Transient { .. }));
    assert!(evotengine::retry::should_retry(&err));
}

#[test]
fn classify_json_404_is_not_retryable() {
    let value = serde_json::json!({
        "error": {
            "type": "not_found_error",
            "message": "model not found"
        }
    });
    let err = classify_json_error(&value);
    assert!(matches!(err, ProviderError::ModelNotFound(_)));
    assert!(!evotengine::retry::should_retry(&err));
}

#[test]
fn classify_json_400_bad_request_is_not_retryable() {
    let value = serde_json::json!({
        "error": {
            "type": "invalid_request_error",
            "message": "Bad request: missing required parameter text"
        }
    });
    let err = classify_json_error(&value);
    assert!(matches!(err, ProviderError::InvalidRequest(_)));
    assert!(!evotengine::retry::should_retry(&err));
}

#[test]
fn classify_openrouter_wrapped_overflow_from_metadata_raw() {
    let value = serde_json::json!({
        "type": "error",
        "error": {
            "type": "invalid_request_error",
            "message": "Provider returned error"
        },
        "metadata": {
            "raw": "{\"error\":{\"message\":\"prompt is too long: 213462 tokens > 200000 maximum\"}}",
            "provider_name": "Stealth"
        }
    });
    let err = classify_json_error(&value);
    assert!(err.is_context_overflow());
    assert!(!evotengine::retry::should_retry(&err));
}

// ---------------------------------------------------------------------------
// StreamResponseKind (via classify_response — tested indirectly through
// the public enum since classify_response takes a reqwest::Response)
// ---------------------------------------------------------------------------

#[test]
fn stream_response_kind_variants() {
    // Just verify the enum is usable
    let streaming = StreamResponseKind::Streaming;
    let json = StreamResponseKind::Json;
    let other = StreamResponseKind::Other("text/plain".into());

    assert_eq!(streaming, StreamResponseKind::Streaming);
    assert_eq!(json, StreamResponseKind::Json);
    assert!(matches!(other, StreamResponseKind::Other(_)));
}

// ---------------------------------------------------------------------------
// check_error_status — gateway hint headers
// ---------------------------------------------------------------------------

async fn error_from_mock_response(template: wiremock::ResponseTemplate) -> ProviderError {
    use wiremock::matchers::method;
    use wiremock::Mock;
    use wiremock::MockServer;

    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(template)
        .mount(&server)
        .await;
    let response = match reqwest::get(server.uri()).await {
        Ok(response) => response,
        Err(error) => panic!("mock request failed: {error}"),
    };
    match check_error_status(response).await {
        Err(error) => error,
        Ok(_) => panic!("expected an error classification"),
    }
}

#[tokio::test]
async fn permanent_protocol_fields_win_over_retry_hints_and_wording() {
    for (status, payload, model_missing) in [
        (
            404,
            serde_json::json!({"error":{"type":"invalid_request_error","code":"model_not_found","message":"Unavailable"}}),
            true,
        ),
        (
            503,
            serde_json::json!({"error":{"type":"invalid_request_error","code":"unsupported_operation","message":"Please retry later"}}),
            false,
        ),
    ] {
        let err = error_from_mock_response(
            wiremock::ResponseTemplate::new(status)
                .insert_header("x-should-retry", "true")
                .set_body_json(payload),
        )
        .await;
        assert!(!evotengine::retry::should_retry(&err));
        assert_eq!(
            matches!(err, ProviderError::ModelNotFound(_)),
            model_missing
        );
    }
    let err = error_from_mock_response(wiremock::ResponseTemplate::new(404).set_body_json(
        serde_json::json!({"error":{"message":"Route not found","type":"not_found_error"}}),
    ))
    .await;
    assert!(matches!(err, ProviderError::InvalidRequest(_)));
    assert!(!evotengine::retry::should_retry(&err));
}

#[tokio::test]
async fn check_error_status_honors_should_retry() {
    // A gateway can mark an unknown 4xx as retryable.
    let err = error_from_mock_response(
        wiremock::ResponseTemplate::new(402)
            .insert_header("x-should-retry", "true")
            .set_body_string("payment required"),
    )
    .await;
    assert!(matches!(err, ProviderError::Transient { .. }));
    assert!(evotengine::retry::should_retry(&err));
}

#[tokio::test]
async fn check_error_status_preserves_nested_rate_limit_detail() {
    let detail = "Rate limit exceeded. Please retry later.";
    let err = error_from_mock_response(wiremock::ResponseTemplate::new(429).set_body_json(
        serde_json::json!({
            "type": "error",
            "error": {"type": "rate_limit_error", "message": detail}
        }),
    ))
    .await;
    match err {
        ProviderError::RateLimited { message } => {
            assert_eq!(
                message,
                "HTTP 429: rate_limit_error: Rate limit exceeded. Please retry later."
            );
        }
        other => panic!("expected RateLimited, got {other:?}"),
    }
}

#[tokio::test]
async fn check_error_status_classifies_code_only_quota_and_overflow() {
    let fatal = error_from_mock_response(wiremock::ResponseTemplate::new(429).set_body_json(
        serde_json::json!({
            "error": {"code": "insufficient_quota", "message": "You exceeded your current quota"}
        }),
    ))
    .await;
    assert!(matches!(fatal, ProviderError::Other(_)));
    assert!(!fatal.is_quota_limited());
    assert!(!evotengine::retry::should_retry(&fatal));
    assert!(fatal.to_string().contains("insufficient_quota"));

    let overflow = error_from_mock_response(wiremock::ResponseTemplate::new(400).set_body_json(
        serde_json::json!({
            "error": {"code": "context_length_exceeded", "message": "too many tokens in request"}
        }),
    ))
    .await;
    assert!(overflow.is_context_overflow());
    assert!(!evotengine::retry::should_retry(&overflow));
}

#[tokio::test]
async fn check_error_status_truncates_oversized_bodies() {
    let oversized = error_from_mock_response(wiremock::ResponseTemplate::new(429).set_body_string(
        format!("{{\"error\":{{\"message\":\"{}\"}}}}", "x".repeat(80_000)),
    ))
    .await;

    assert!(oversized.to_string().contains('…'));
    assert!(oversized.to_string().len() < 80_000);
}

// ---------------------------------------------------------------------------
// send_json_stream_request: gzip only where the origin has advertised it
// ---------------------------------------------------------------------------

mod request_compression {
    use std::io::Read;

    use evotengine::provider::stream::http::forget_request_encoding;
    use evotengine::provider::stream::http::origin_accepts_gzip;
    use evotengine::provider::stream::http::send_json_stream_request;
    use wiremock::matchers::header;
    use wiremock::matchers::method;
    use wiremock::Mock;
    use wiremock::MockServer;
    use wiremock::ResponseTemplate;

    fn big_body() -> serde_json::Value {
        serde_json::json!({"model": "m", "messages": [{"role": "user", "content": "x".repeat(200_000)}]})
    }

    fn gunzip(bytes: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        flate2::read::GzDecoder::new(bytes)
            .read_to_end(&mut out)
            .unwrap();
        out
    }

    async fn send(server: &MockServer, body: &serde_json::Value) -> u16 {
        let url = format!("{}/v1/messages", server.uri());
        let builder = reqwest::Client::new()
            .post(&url)
            .header("content-type", "application/json");
        send_json_stream_request(builder, &url, body.clone())
            .await
            .unwrap()
            .status()
            .as_u16()
    }

    fn advertising() -> ResponseTemplate {
        ResponseTemplate::new(200).insert_header("accept-encoding", "gzip, deflate")
    }

    #[tokio::test]
    async fn plain_until_the_origin_advertises_then_gzip() {
        let server = MockServer::start().await;
        forget_request_encoding(&server.uri());
        Mock::given(method("POST"))
            .respond_with(advertising())
            .mount(&server)
            .await;
        let body = big_body();
        send(&server, &body).await;
        send(&server, &body).await;
        let seen = server.received_requests().await.unwrap();
        assert!(
            seen[0].headers.get("content-encoding").is_none(),
            "first request cannot know yet"
        );
        assert_eq!(seen[1].headers.get("content-encoding").unwrap(), "gzip");
        assert!(
            seen[1].body.len() * 10 < seen[0].body.len(),
            "the upload shrinks"
        );
        assert_eq!(gunzip(&seen[1].body), seen[0].body, "same JSON either way");
    }

    #[tokio::test]
    async fn an_origin_that_never_advertises_always_gets_plain_json() {
        let server = MockServer::start().await;
        forget_request_encoding(&server.uri());
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;
        let body = big_body();
        for _ in 0..3 {
            send(&server, &body).await;
        }
        let seen = server.received_requests().await.unwrap();
        assert!(seen
            .iter()
            .all(|r| r.headers.get("content-encoding").is_none()));
        assert!(!origin_accepts_gzip(&server.uri()));
    }

    #[tokio::test]
    async fn small_bodies_are_not_compressed_even_where_accepted() {
        let server = MockServer::start().await;
        forget_request_encoding(&server.uri());
        Mock::given(method("POST"))
            .respond_with(advertising())
            .mount(&server)
            .await;
        let small =
            serde_json::json!({"model": "m", "messages": [{"role": "user", "content": "hi"}]});
        send(&server, &small).await;
        send(&server, &small).await;
        let seen = server.received_requests().await.unwrap();
        assert!(seen
            .iter()
            .all(|r| r.headers.get("content-encoding").is_none()));
    }

    #[tokio::test]
    async fn a_415_to_gzip_resends_plain_and_forgets_the_origin() {
        let server = MockServer::start().await;
        forget_request_encoding(&server.uri());
        Mock::given(method("POST"))
            .and(header("content-encoding", "gzip"))
            .respond_with(ResponseTemplate::new(415))
            .with_priority(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .respond_with(advertising())
            .with_priority(2)
            .mount(&server)
            .await;
        let body = big_body();
        assert_eq!(send(&server, &body).await, 200); // plain; origin advertises
        assert_eq!(send(&server, &body).await, 200); // gzip -> 415 -> plain again
        let seen = server.received_requests().await.unwrap();
        let encodings: Vec<_> = seen
            .iter()
            .map(|r| {
                r.headers
                    .get("content-encoding")
                    .map(|v| v.to_str().unwrap().to_string())
            })
            .collect();
        assert_eq!(encodings, vec![None, Some("gzip".into()), None]);
        // The plain resend was answered with accept-encoding again, so it is relearned;
        // what matters is that the caller got a 200, not the 415.
    }
}
