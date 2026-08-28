//! Media and sensor inputs. Everything here produces encoded bytes; nothing
//! decodes. The daemon depacketizes (RTP → NAL units) — that is framing, not
//! transcoding.

pub mod rtp;
pub mod rtsp;
pub mod udp;

use bytes::Bytes;

/// One encoded picture (Annex B, parameter sets inline on keyframes).
#[derive(Debug, Clone)]
pub struct VideoAu {
    pub data: Bytes,
    pub keyframe: bool,
    /// Agent monotonic microseconds at which the last byte arrived.
    pub capture_ts_us: u64,
    /// RTP packets lost before this picture (input-side loss, not Seyd's).
    pub input_loss: u16,
}

/// Monotonic microseconds since process start — the agent clock used for
/// `send_ts`, `capture_ts_us` and `pong.t2`.
pub fn now_us() -> u64 {
    use std::sync::OnceLock;
    use std::time::Instant;
    static EPOCH: OnceLock<Instant> = OnceLock::new();
    EPOCH.get_or_init(Instant::now).elapsed().as_micros() as u64
}

/// Strip userinfo from a URL for logging. libavformat taught us that the
/// password leaks on the *error* path if this is not applied everywhere.
pub fn redact(url: &str) -> String {
    match url::Url::parse(url) {
        Ok(mut u) if u.username() != "" || u.password().is_some() => {
            let _ = u.set_username("");
            let _ = u.set_password(None);
            u.to_string()
        }
        _ => url.to_string(),
    }
}

/// Start the video input matching the URL scheme; frames flow to `tx`.
/// Reconnects forever with backoff — a robot whose camera reboots keeps its
/// agent alive.
pub fn spawn_video(url: String, tx: tokio::sync::mpsc::Sender<VideoAu>) -> anyhow::Result<()> {
    let parsed = url::Url::parse(&url)?;
    match parsed.scheme() {
        "rtsp" | "rtsps" => {
            tokio::spawn(rtsp::run(url, tx));
        }
        "rtp" => {
            let addr = format!(
                "{}:{}",
                parsed.host_str().unwrap_or("0.0.0.0"),
                parsed
                    .port()
                    .ok_or_else(|| anyhow::anyhow!("rtp:// needs a port"))?
            );
            tokio::spawn(rtp::run(addr, tx));
        }
        other => anyhow::bail!("unsupported video input scheme {other:?}"),
    }
    Ok(())
}
