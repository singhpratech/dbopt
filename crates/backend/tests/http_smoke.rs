//! End-to-end HTTP smoke test for the backend server.
//!
//! The router lives inside the `sqlopt-backend` binary crate, whose modules are
//! private and not importable from an integration test. So instead of rebuilding
//! the router here, we spawn the actual compiled binary
//! (`env!("CARGO_BIN_EXE_sqlopt-backend")` is provided by Cargo for integration
//! tests) on a fixed port, poll `/api/health` until it is accepting, then
//! exercise the real HTTP surface with reqwest. This tests the genuine artifact.

use std::process::{Child, Command};
use std::time::{Duration, Instant};

const PORT: u16 = 39123;

/// Kills the spawned server even if an assertion panics mid-test.
struct ServerGuard(Child);

impl Drop for ServerGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn base() -> String {
    format!("http://127.0.0.1:{PORT}")
}

async fn wait_until_ready(client: &reqwest::Client, timeout: Duration) {
    let url = format!("{}/api/health", base());
    let start = Instant::now();
    loop {
        if let Ok(resp) = client.get(&url).send().await {
            if resp.status().is_success() {
                return;
            }
        }
        if start.elapsed() > timeout {
            panic!("backend did not become ready within {timeout:?}");
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
}

#[tokio::test]
async fn http_smoke_end_to_end() {
    // 1. Spawn the real built binary on a dedicated port.
    let child = Command::new(env!("CARGO_BIN_EXE_sqlopt-backend"))
        .env("PORT", PORT.to_string())
        .env("SQLOPT_NO_OPEN", "1")
        .spawn()
        .expect("failed to spawn sqlopt-backend binary");
    let _guard = ServerGuard(child);

    let client = reqwest::Client::new();

    // 2. Poll /api/health until it answers 200 (or time out).
    wait_until_ready(&client, Duration::from_secs(10)).await;

    // 3. /api/health body should be exactly "ok".
    let health = client
        .get(format!("{}/api/health", base()))
        .send()
        .await
        .expect("health request failed");
    assert_eq!(health.status(), reqwest::StatusCode::OK);
    let health_body = health.text().await.expect("read health body");
    assert_eq!(health_body, "ok", "unexpected /api/health body");

    // 4. /api/analyze should flag select_star + nolock on this query.
    let analyze = client
        .post(format!("{}/api/analyze", base()))
        .json(&serde_json::json!({
            "sql": "SELECT * FROM Orders WITH (NOLOCK);",
            "server_version": 2025
        }))
        .send()
        .await
        .expect("analyze request failed");
    assert_eq!(analyze.status(), reqwest::StatusCode::OK);
    let analyze_json: serde_json::Value = analyze.json().await.expect("analyze json");
    let findings = analyze_json
        .get("findings")
        .and_then(|f| f.as_array())
        .expect("analyze response missing findings array");
    assert!(
        !findings.is_empty(),
        "expected non-empty findings for SELECT * + NOLOCK, got: {analyze_json}"
    );

    // 5. AI log: POST one interaction, then confirm it shows up in GET.
    let ai_id = format!("smoke-ai-{}", uuid_like());
    let ai_post = client
        .post(format!("{}/api/logs/ai", base()))
        .json(&serde_json::json!({
            "id": ai_id,
            "provider": "ollama",
            "model": "gemma4:e4b",
            "user_prompt": "explain this plan",
            "response": "looks fine",
            "status": "ok"
        }))
        .send()
        .await
        .expect("post ai log failed");
    assert_eq!(ai_post.status(), reqwest::StatusCode::OK, "post ai log non-200");

    let ai_get = client
        .get(format!("{}/api/logs/ai?limit=5", base()))
        .send()
        .await
        .expect("get ai log failed");
    assert_eq!(ai_get.status(), reqwest::StatusCode::OK);
    let ai_json: serde_json::Value = ai_get.json().await.expect("ai log json");
    let entries = ai_json
        .get("entries")
        .and_then(|e| e.as_array())
        .expect("ai log response missing entries array");
    assert!(
        entries
            .iter()
            .any(|e| e.get("id").and_then(|v| v.as_str()) == Some(ai_id.as_str())),
        "posted ai log id {ai_id} not found in: {ai_json}"
    );

    // 6. Analysis run: POST a minimal run, then confirm it appears in GET.
    let run_id = format!("smoke-run-{}", uuid_like());
    let run_post = client
        .post(format!("{}/api/logs/analysis", base()))
        .json(&serde_json::json!({
            "id": run_id,
            "mode": "adhoc",
            "findings_total": 0,
            "findings_critical": 0,
            "findings_error": 0,
            "findings_warning": 0,
            "findings_info": 0,
            "plan_attached": false,
            "findings": []
        }))
        .send()
        .await
        .expect("post analysis run failed");
    assert_eq!(
        run_post.status(),
        reqwest::StatusCode::OK,
        "post analysis run non-200"
    );

    let run_get = client
        .get(format!("{}/api/logs/analysis", base()))
        .send()
        .await
        .expect("get analysis runs failed");
    assert_eq!(run_get.status(), reqwest::StatusCode::OK);
    let run_json: serde_json::Value = run_get.json().await.expect("analysis runs json");
    let runs = run_json
        .get("runs")
        .and_then(|r| r.as_array())
        .expect("analysis runs response missing runs array");
    assert!(
        runs.iter()
            .any(|r| r.get("id").and_then(|v| v.as_str()) == Some(run_id.as_str())),
        "posted analysis run id {run_id} not found in: {run_json}"
    );

    // 7. _guard's Drop kills the child here (and on any panic above).
}

/// Cheap unique-ish suffix so reruns don't rely on a fresh DB. We avoid pulling
/// in a uuid crate dependency for the test by using the nanosecond clock.
fn uuid_like() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}
