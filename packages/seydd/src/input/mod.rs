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
    /// Microseconds on the *source's* sampling clock (the RTP timestamp),
    /// relative to the first picture of the stream — not an agent-clock
    /// arrival time. The pilot paces presentation on this timeline, so it must
    /// carry the encoder's cadence and none of the transport's jitter.
    pub capture_ts_us: u64,
    /// RTP packets lost before this picture (input-side loss, not Seyd's).
    pub input_loss: u16,
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

/// Which elementary stream the input carries.
///
/// Seyd never decodes video, so this exists for exactly two reasons: to find
/// frame boundaries in an RTP stream, and to know which frames are keyframes.
/// Everything downstream — chunking, FEC, transport, pacing — is bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Codec {
    #[default]
    H264,
    H265,
    /// Motion JPEG: a sequence of complete JPEG images, one per frame.
    ///
    /// The only codec ONVIF Profile S makes *mandatory*, so every conformant
    /// camera can emit it while none is obliged to emit H.265 — which makes it
    /// the compatibility path, not a preference. It costs roughly an order of
    /// magnitude more bandwidth than H.264.
    Mjpeg,
}

impl Codec {
    /// From the channel's `codec` string, which is a WebCodecs identifier
    /// (`avc1.42001f`, `hev1.1.6.L93.B0`) because it also travels to the
    /// browser's decoder untouched. Bare `h264` / `h265` are accepted too.
    pub fn from_codec_string(s: &str) -> anyhow::Result<Self> {
        let head = s.split('.').next().unwrap_or(s).to_ascii_lowercase();
        match head.as_str() {
            "avc1" | "avc3" | "h264" => Ok(Codec::H264),
            "hev1" | "hvc1" | "h265" | "hevc" => Ok(Codec::H265),
            "mjpeg" | "jpeg" => Ok(Codec::Mjpeg),
            _ => anyhow::bail!(
                "codec {s:?} is not a video codec Seyd can frame. Use an H.264 \
                 (avc1.…), H.265 (hev1.…) or Motion JPEG (mjpeg) identifier."
            ),
        }
    }

    /// The RTP `encoding_name` an RTSP server advertises for this codec.
    pub fn rtp_encoding_name(self) -> &'static str {
        match self {
            Codec::H264 => "h264",
            Codec::H265 => "h265",
            Codec::Mjpeg => "jpeg",
        }
    }
}

/// Start the video input matching the URL scheme; frames flow to `tx`.
/// Reconnects forever with backoff — a robot whose camera reboots keeps its
/// agent alive.
pub fn spawn_video(
    url: String,
    codec: Codec,
    tx: tokio::sync::mpsc::Sender<VideoAu>,
) -> anyhow::Result<()> {
    if codec == Codec::Mjpeg {
        // Measured on a 640x480@15 test source: 3478 kbps, against 3431 kbps
        // for H.264 at 1280x720@30 — the same bandwidth for an eighth of the
        // pixel throughput. Worth saying out loud, because a camera left on
        // its ONVIF-mandated default will quietly cost eight times the link.
        tracing::warn!(
            "channel is Motion JPEG: every frame is a full JPEG, so expect roughly \
             an order of magnitude more bandwidth than H.264 at the same picture. \
             Use it for cameras that offer nothing better, and prefer H.264 or \
             H.265 where the camera supports it."
        );
    }
    let parsed = url::Url::parse(&url)?;
    match parsed.scheme() {
        "rtsp" | "rtsps" => {
            tokio::spawn(rtsp::run(url, codec, tx));
        }
        // RFC 2435 strips the JPEG headers from every packet and the receiver
        // has to rebuild a JFIF header from the type, Q and dimensions. Retina
        // does that for us on the RTSP path; doing it again here for bare RTP
        // would be a second implementation of a fiddly reconstruction for a
        // transport no ONVIF camera uses. Say so rather than half-support it.
        "rtp" if codec == Codec::Mjpeg => {
            anyhow::bail!(
                "Motion JPEG is supported over rtsp:// only. Bare rtp:// carries \
                 JPEG without its headers (RFC 2435) and Seyd does not rebuild them; \
                 point this channel at the camera's RTSP URL, or have the source \
                 send H.264."
            )
        }
        "rtp" => {
            let addr = format!(
                "{}:{}",
                parsed.host_str().unwrap_or("0.0.0.0"),
                parsed
                    .port()
                    .ok_or_else(|| anyhow::anyhow!("rtp:// needs a port"))?
            );
            tokio::spawn(rtp::run(addr, codec, tx));
        }
        other => anyhow::bail!("unsupported video input scheme {other:?}"),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codec_strings_map_to_the_right_framing() {
        // WebCodecs identifiers, because the same string travels to the
        // browser's decoder untouched.
        assert_eq!(
            Codec::from_codec_string("avc1.42001f").unwrap(),
            Codec::H264
        );
        assert_eq!(
            Codec::from_codec_string("hev1.1.6.L93.B0").unwrap(),
            Codec::H265
        );
        assert_eq!(
            Codec::from_codec_string("hvc1.1.6.L93.B0").unwrap(),
            Codec::H265
        );
        assert_eq!(Codec::from_codec_string("mjpeg").unwrap(), Codec::Mjpeg);
        assert_eq!(Codec::from_codec_string("JPEG").unwrap(), Codec::Mjpeg);
        assert!(Codec::from_codec_string("vp09.00.10.08").is_err());
    }

    #[test]
    fn rtp_encoding_names_match_what_an_rtsp_server_advertises() {
        assert_eq!(Codec::H264.rtp_encoding_name(), "h264");
        assert_eq!(Codec::H265.rtp_encoding_name(), "h265");
        // retina matches ("image" | "video", "jpeg").
        assert_eq!(Codec::Mjpeg.rtp_encoding_name(), "jpeg");
    }

    #[test]
    fn bare_rtp_mjpeg_is_refused_with_an_explanation_rather_than_silence() {
        // RFC 2435 strips the JPEG headers; rebuilding them is retina's job on
        // the RTSP path and Seyd does not duplicate it. Failing loudly beats
        // accepting the config and delivering no frames.
        let (tx, _rx) = tokio::sync::mpsc::channel(1);
        let err = spawn_video("rtp://127.0.0.1:5000".into(), Codec::Mjpeg, tx)
            .expect_err("bare rtp:// MJPEG must be refused");
        let msg = err.to_string();
        assert!(
            msg.contains("rtsp://"),
            "the error should say what to do instead: {msg}"
        );
    }

    // Needs a runtime: unlike the MJPEG case this reaches `tokio::spawn`.
    #[tokio::test]
    async fn bare_rtp_still_works_for_the_codecs_that_carry_their_own_headers() {
        let (tx, _rx) = tokio::sync::mpsc::channel(1);
        assert!(spawn_video("rtp://127.0.0.1:5000".into(), Codec::H264, tx).is_ok());
    }
}
