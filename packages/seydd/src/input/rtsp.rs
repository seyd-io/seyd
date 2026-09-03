//! RTSP input via `retina`, pulled over TCP.
//!
//! TCP rather than UDP on purpose: an IP camera's RTP/UDP has no FEC and its
//! losses would arrive as corrupt access units that Seyd then spends parity
//! protecting. The camera hop is a short LAN link where a retransmit costs
//! microseconds; the lossy path worth protecting is the one after the agent.

use super::{redact, VideoAu};
use bytes::Bytes;
use futures_util::StreamExt;
use retina::client::{PlayOptions, SessionOptions, SetupOptions, Transport};
use retina::codec::{CodecItem, FrameFormat};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

const BASE_BACKOFF: Duration = Duration::from_secs(1);
const MAX_BACKOFF: Duration = Duration::from_secs(30);
/// A stream that ran this long was healthy, not flapping, so the failure that
/// ends it starts the backoff over. Without this a camera that drops after
/// hours costs the operator `MAX_BACKOFF` of black screen instead of a second.
const STABLE_SESSION: Duration = Duration::from_secs(30);

pub async fn run(url: String, tx: mpsc::Sender<VideoAu>) {
    let mut backoff = BASE_BACKOFF;
    loop {
        let started = Instant::now();
        let outcome = session(&url, &tx).await;
        // Reset before logging, so the delay reported is the one actually slept.
        if started.elapsed() >= STABLE_SESSION {
            backoff = BASE_BACKOFF;
        }
        match outcome {
            Ok(()) => tracing::info!(url = %redact(&url), "rtsp stream ended; reconnecting"),
            Err(e) => {
                tracing::warn!(url = %redact(&url), error = %redact(&e.to_string()), "rtsp input failed; retrying in {backoff:?}")
            }
        }
        if tx.is_closed() {
            return;
        }
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(MAX_BACKOFF);
    }
}

/// Substitute `SEYD_RTSP_USER` / `SEYD_RTSP_PASSWORD` into a URL without userinfo.
pub fn resolve_credentials(url: &str) -> String {
    let Ok(mut u) = url::Url::parse(url) else {
        return url.to_string();
    };
    if !u.username().is_empty() {
        return url.to_string();
    }
    if let (Ok(user), Ok(pass)) = (
        std::env::var("SEYD_RTSP_USER"),
        std::env::var("SEYD_RTSP_PASSWORD"),
    ) {
        let _ = u.set_username(&user);
        let _ = u.set_password(Some(&pass));
    }
    u.to_string()
}

async fn session(url: &str, tx: &mpsc::Sender<VideoAu>) -> anyhow::Result<()> {
    let mut parsed = url::Url::parse(url)?;
    let creds = if !parsed.username().is_empty() {
        let c = retina::client::Credentials {
            username: parsed.username().to_string(),
            password: parsed.password().unwrap_or("").to_string(),
        };
        let _ = parsed.set_username("");
        let _ = parsed.set_password(None);
        Some(c)
    } else {
        None
    };

    let mut sess = retina::client::Session::describe(
        parsed,
        SessionOptions::default()
            .creds(creds)
            .user_agent("seydd".into()),
    )
    .await?;

    // Video only: a camera carrying audio would otherwise hand us AAC frames
    // that a VideoDecoder would be fed as delta frames.
    let video_idx = sess
        .streams()
        .iter()
        .position(|s| s.media() == "video" && s.encoding_name().eq_ignore_ascii_case("h264"))
        .ok_or_else(|| anyhow::anyhow!("no H.264 video stream in RTSP DESCRIBE"))?;
    sess.setup(
        video_idx,
        SetupOptions::default()
            .transport(Transport::Tcp(Default::default()))
            .frame_format(FrameFormat::SIMPLE),
    )
    .await?;
    let playing = sess.play(PlayOptions::default()).await?;
    let mut demuxed = playing.demuxed()?;
    tracing::info!(url = %redact(url), "rtsp input playing");

    let mut frames: u64 = 0;
    let mut last_at = std::time::Instant::now();
    while let Some(item) = demuxed.next().await {
        let gap = last_at.elapsed();
        last_at = std::time::Instant::now();
        if gap.as_millis() > 120 {
            tracing::debug!(gap_ms = gap.as_millis() as u64, "rtsp inter-frame gap");
        }
        if let CodecItem::VideoFrame(f) = item? {
            {
                if f.stream_id() != video_idx {
                    continue;
                }
                // The camera's own sampling clock (RTP timestamp), not our
                // arrival time: the pilot paces presentation on this timeline,
                // and arrival already carries the jitter we are trying to
                // remove — a keyframe reaches us several ms after its slot.
                let ts = f.timestamp();
                let capture_ts_us = (ts.elapsed().max(0) as u128 * 1_000_000
                    / ts.clock_rate().get() as u128) as u64;
                let au = VideoAu {
                    keyframe: f.is_random_access_point(),
                    input_loss: f.loss(),
                    capture_ts_us,
                    data: Bytes::from(f.into_data()),
                };
                frames += 1;
                if frames == 1 {
                    tracing::info!(
                        bytes = au.data.len(),
                        keyframe = au.keyframe,
                        "first rtsp frame"
                    );
                }
                if tx.send(au).await.is_err() {
                    return Ok(());
                }
            }
        }
    }
    Ok(())
}
