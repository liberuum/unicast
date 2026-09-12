use crate::subtitles::SubtitleTrack;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionState {
    Idle,
    Connecting,
    Buffering,
    Playing,
    Paused,
    Stopped,
    Error,
}

impl SessionState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Connecting => "connecting",
            Self::Buffering => "buffering",
            Self::Playing => "playing",
            Self::Paused => "paused",
            Self::Stopped => "stopped",
            Self::Error => "error",
        }
    }

    pub fn is_active(self) -> bool {
        !matches!(self, Self::Idle | Self::Stopped)
    }
}

#[derive(Debug, Clone)]
#[allow(clippy::struct_field_names)]
pub struct Session {
    pub session_id: u64,
    pub state: SessionState,
    pub ip: String,
    pub name: String,
    pub protocol: String,
    pub file: String,
    pub position: f64,
    pub duration: f64,
    pub error: String,
    /// Receiver volume 0-100, -1 while unknown.
    pub volume: i32,
    pub muted: bool,
    /// Whether this receiver/protocol lets us change its volume.
    pub volume_supported: bool,
    /// Audio boost (dB) applied to the stream being served.
    pub boost: i32,
    /// Subtitle tracks available for the file, and the one the receiver shows.
    pub subtitles: Vec<SubtitleTrack>,
    pub subtitle: Option<u32>,
    /// Whether this protocol can show sidecar subtitles (Google Cast only).
    pub subtitles_supported: bool,
}

impl Default for Session {
    fn default() -> Self {
        Self {
            session_id: 0,
            state: SessionState::Idle,
            ip: String::new(),
            name: String::new(),
            protocol: String::new(),
            file: String::new(),
            position: 0.0,
            duration: 0.0,
            error: String::new(),
            volume: -1,
            muted: false,
            volume_supported: false,
            boost: 0,
            subtitles: Vec::new(),
            subtitle: None,
            subtitles_supported: false,
        }
    }
}

impl Session {
    pub fn new_id(session_id: u64) -> Self {
        Self {
            session_id,
            ..Self::default()
        }
    }
}
