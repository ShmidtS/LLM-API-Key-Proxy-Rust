//! One-off diagnostic: probe zai's quota endpoint for every ZAI_API_KEY_*
//! found in the workspace `.env` and print remaining/nextResetTime per key.
//! Run: cargo run -p rotator --example check_zai_keys

use std::fs;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const QUOTA_ENDPOINT: &str = "https://api.z.ai/api/monitor/usage/quota/limit";

fn load_zai_keys() -> Vec<(String, String)> {
    let env = fs::read_to_string("../../.env")
        .or_else(|_| fs::read_to_string(".env"))
        .expect("cannot read .env");
    let mut keys = Vec::new();
    for line in env.lines() {
        let line = line.trim();
        if line.starts_with('#') || !line.starts_with("ZAI_API_KEY_") {
            continue;
        }
        if let Some((name, value)) = line.split_once('=') {
            let value = value.trim().trim_matches('"');
            if !value.is_empty() {
                keys.push((name.to_string(), value.to_string()));
            }
        }
    }
    keys.sort_by(|a, b| a.0.cmp(&b.0));
    keys
}

fn fmt_reset(ms: Option<i64>) -> String {
    ms.map(|m| {
        let secs = m / 1000;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        let delta = secs - now;
        if delta <= 0 {
            "already-reset".to_string()
        } else {
            format!("in {}h{}m ({}s)", delta / 3600, (delta % 3600) / 60, delta)
        }
    })
    .unwrap_or_else(|| "n/a".to_string())
}

#[tokio::main]
async fn main() {
    let keys = load_zai_keys();
    println!("found {} zai keys\n", keys.len());
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .build()
        .unwrap();

    for (name, key) in &keys {
        let res = client
            .get(QUOTA_ENDPOINT)
            .header("Authorization", format!("Bearer {key}"))
            .header("Accept", "application/json")
            .send()
            .await;
        match res {
            Ok(resp) => {
                let body: serde_json::Value = match resp.json().await {
                    Ok(b) => b,
                    Err(e) => {
                        println!("{name} | unparseable body: {e}");
                        continue;
                    }
                };
                let code = body.get("code").and_then(|c| c.as_i64()).unwrap_or(-1);
                let tl = body
                    .pointer("/data/limits")
                    .and_then(|l| l.as_array())
                    .and_then(|arr| {
                        arr.iter().find(|x| {
                            x.get("type").and_then(|t| t.as_str()) == Some("TIME_LIMIT")
                                && x.get("unit").and_then(|u| u.as_i64()) == Some(5)
                        })
                    });
                match tl {
                    Some(tl) => {
                        let remaining = tl.get("remaining").and_then(|r| r.as_i64());
                        let usage = tl.get("usage").and_then(|u| u.as_i64());
                        let cur = tl.get("currentValue").and_then(|u| u.as_i64());
                        let reset = tl.get("nextResetTime").and_then(|r| r.as_i64());
                        println!(
                            "{name} | code={code} | remaining={remaining:?} usage={usage:?} cur={cur:?} | reset {}",
                            fmt_reset(reset)
                        );
                    }
                    None => println!("{name} | code={code} | no TIME_LIMIT/unit5 in response"),
                }
            }
            Err(e) => println!("{name} | HTTP error: {e}"),
        }
    }
}
