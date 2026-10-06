// Copyright 2026 Anton Gravestam
// SPDX-License-Identifier: Apache-2.0
//! `seydd enrol --token …` — redeem an enrolment token issued by the console.
//!
//! This is the robot half of the enrolment loop (PLAN.md §2.5, ADR 0007). The
//! robot generates its own Ed25519 key, keeps the private half, and sends only
//! the public half with a one-time token. It never sees a user credential and
//! never talks to the identity provider: a fleet enrols, and keeps running,
//! independently of whoever the humans authenticate against.

use anyhow::{bail, Context};
use seyd_signal_client::Identity;
use std::path::Path;
use url::Url;

/// Turns the signal WebSocket URL into the API's origin:
/// `wss://signal.seyd.io/ws` → `https://signal.seyd.io`.
pub fn api_base(signal_url: &str) -> anyhow::Result<Url> {
    let mut url =
        Url::parse(signal_url).with_context(|| format!("bad signal_url: {signal_url}"))?;
    let scheme = match url.scheme() {
        "wss" | "https" => "https",
        "ws" | "http" => "http",
        other => bail!("signal_url has scheme {other:?}; expected ws, wss, http or https"),
    };
    url.set_scheme(scheme)
        .ok()
        .context("could not rewrite the URL scheme")?;
    url.set_path("");
    url.set_query(None);
    url.set_fragment(None);
    Ok(url)
}

pub async fn run(
    robot_id: &str,
    signal_url: &str,
    credential_path: &Path,
    token: &str,
) -> anyhow::Result<()> {
    // load_or_create: enrolling twice with the same credential file re-presents
    // the same key rather than orphaning the first one.
    let identity = Identity::load_or_create(credential_path)
        .with_context(|| format!("could not open {}", credential_path.display()))?;
    let public_key = identity.public_key_b64();

    let url = api_base(signal_url)?.join("api/v1/enrol")?;
    tracing::info!(robot_id, %url, "enrolling");

    let res = reqwest::Client::new()
        .post(url.clone())
        .json(&serde_json::json!({
            "token": token,
            "robot_id": robot_id,
            "public_key": public_key,
        }))
        .send()
        .await
        .with_context(|| format!("could not reach {url}"))?;

    let status = res.status();
    let body = res.text().await.unwrap_or_default();
    if status.is_success() {
        println!("Enrolled {robot_id}.");
        println!("Key: {public_key}");
        println!("Credential: {}", credential_path.display());
        println!("\nStart the robot with: seydd --config <your seydd.toml>");
        return Ok(());
    }

    // The server's codes are deliberately distinguishable; say what to do next
    // rather than printing a status line.
    let detail: serde_json::Value = serde_json::from_str(&body).unwrap_or_default();
    let code = detail.get("error").and_then(|v| v.as_str()).unwrap_or("");
    match (status.as_u16(), code) {
        (410, _) => bail!(
            "this enrolment token cannot be used: it is expired, already redeemed, \
             or the robot id {robot_id:?} is taken. Issue a new one in the console."
        ),
        (400, _) => bail!(
            "the server rejected the request: {}",
            detail
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or(&body)
        ),
        (404, _) => bail!(
            "{url} returned 404 — is signal_url pointing at a Seyd signal server \
             new enough to have enrolment?"
        ),
        _ => bail!("enrolment failed ({status}): {body}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derives_the_api_origin_from_the_signal_url() {
        assert_eq!(
            api_base("wss://signal.seyd.io/ws").unwrap().as_str(),
            "https://signal.seyd.io/"
        );
        assert_eq!(
            api_base("ws://localhost:8080/ws").unwrap().as_str(),
            "http://localhost:8080/"
        );
        // Query strings and paths on the signal URL must not leak into the API call.
        assert_eq!(
            api_base("ws://host:1/ws?x=1#f").unwrap().as_str(),
            "http://host:1/"
        );
        assert!(api_base("ftp://nope/ws").is_err());
    }

    #[test]
    fn joins_the_enrol_path_onto_the_origin() {
        let url = api_base("wss://signal.seyd.io/ws")
            .unwrap()
            .join("api/v1/enrol")
            .unwrap();
        assert_eq!(url.as_str(), "https://signal.seyd.io/api/v1/enrol");
    }
}
