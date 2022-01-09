use crate::utils::{serde_from_str, serde_option_from_str};
use serde::Deserialize;

#[derive(Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Version {
    pub enabled_features: Vec<String>,
    pub version: String,
}

/// https://aria2.github.io/manual/en/html/aria2c.html#aria2.tellStatus
#[derive(Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    /// GID of the download.
    pub gid: String,
    pub status: TaskStatus,
    #[serde(with = "serde_from_str")]
    pub total_length: u64,
    #[serde(with = "serde_from_str")]
    pub completed_length: u64,
    #[serde(with = "serde_from_str")]
    pub upload_length: u64,
    /// Hexadecimal representation of the download progress.
    ///
    /// The highest bit corresponds to the piece at index 0.
    ///
    /// Any set bits indicate loaded pieces, while
    /// unset bits indicate not yet loaded and/or missing pieces.
    ///
    /// Any overflow bits at the end are set to zero.
    ///
    /// When the download was not started yet,
    /// this key will not be included in the response.
    pub bitfield: String,
    #[serde(with = "serde_from_str")]
    pub download_speed: u64,
    #[serde(with = "serde_from_str")]
    pub upload_speed: u64,
    /// InfoHash. BitTorrent only
    pub info_hash: Option<String>,
    #[serde(with = "serde_option_from_str", default)]
    pub num_seeders: Option<u64>,
    /// true if the local endpoint is a seeder. Otherwise false. BitTorrent only.
    pub seeder: Option<bool>,
    #[serde(with = "serde_from_str")]
    pub piece_length: u64,
    #[serde(with = "serde_from_str")]
    pub num_pieces: u64,
    #[serde(with = "serde_from_str")]
    pub connections: u64,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
    /// List of GIDs which are generated as the result of this download.
    ///
    /// For example, when aria2 downloads a Metalink file,
    /// it generates downloads described in the Metalink (see the --follow-metalink option).
    ///
    /// This value is useful to track auto-generated downloads.
    ///
    /// If there are no such downloads, this key will not be included in the response.
    pub followed_by: Option<Vec<String>>,
    /// The reverse link for followedBy.
    ///
    /// A download included in followedBy has this object's GID in its following value.
    pub following: Option<String>,
    /// GID of a parent download.
    ///
    /// Some downloads are a part of another download.
    ///
    /// For example, if a file in a Metalink has BitTorrent resources,
    /// the downloads of ".torrent" files are parts of that parent.
    ///
    /// If this download has no parent, this key will not be included in the response.
    pub belongs_to: Option<String>,
    pub dir: String,
    pub files: Vec<File>,
    pub bittorrent: Option<BittorrentStatus>,
    /// The number of verified number of bytes while the files are being hash checked.
    ///
    /// This key exists only when this download is being hash checked.
    #[serde(with = "serde_option_from_str", default)]
    pub verified_length: Option<u64>,
    /// `true` if this download is waiting for the hash check in a queue.
    ///
    /// This key exists only when this download is in the queue.
    #[serde(with = "serde_option_from_str", default)]
    pub verify_integrity_pending: Option<bool>,
}

#[derive(Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct BittorrentStatus {
    pub announce_list: Vec<String>,
    pub comment: Option<String>,
    #[serde(with = "serde_from_str")]
    pub creation_date: u64,
    pub mode: BitTorrentFileMode,
}

#[derive(Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum BitTorrentFileMode {
    Single,
    Multi,
}

#[derive(Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct File {
    #[serde(with = "serde_from_str")]
    index: u64,
    path: String,
    #[serde(with = "serde_from_str")]
    length: u64,
    #[serde(with = "serde_from_str")]
    completed_length: u64,
    #[serde(with = "serde_from_str")]
    selected: bool,
    uris: Vec<Uri>,
}

#[derive(Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Uri {
    status: UriStatus,
    uri: String,
}

#[derive(Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum UriStatus {
    Used,
    Waiting,
}

/// `Active` for currently downloading/seeding downloads.
///
/// `Waiting` for downloads in the queue; download is not started.
///
/// `Paused` for paused downloads.
///
/// `Error` for downloads that were stopped because of error.
///
/// `Complete` for stopped and completed downloads.
///
/// `Removed` for the downloads removed by user.
///
/// https://aria2.github.io/manual/en/html/aria2c.html#aria2.tellStatus
#[derive(Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum TaskStatus {
    Active,
    Waiting,
    Paused,
    Error,
    Complete,
    Removed,
}

#[derive(Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Peer {
    #[serde(with = "serde_from_str")]
    pub am_choking: bool,
    pub bitfield: String,
    #[serde(with = "serde_from_str")]
    pub download_speed: u64,
    pub ip: String,
    #[serde(with = "serde_from_str")]
    pub peer_choking: bool,
    pub peer_id: String,
    #[serde(with = "serde_from_str")]
    pub port: u16,
    #[serde(with = "serde_from_str")]
    pub seeder: bool,
    #[serde(with = "serde_from_str")]
    pub upload_speed: u64,
}

#[derive(Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct GlobalStat {
    #[serde(with = "serde_from_str")]
    download_speed: u64,
    #[serde(with = "serde_from_str")]
    upload_speed: u64,
    #[serde(with = "serde_from_str")]
    num_active: i32,
    #[serde(with = "serde_from_str")]
    num_waiting: i32,
    #[serde(with = "serde_from_str")]
    num_stopped: i32,
    #[serde(with = "serde_from_str")]
    num_stopped_total: i32,
}

#[derive(Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SessionInfo {
    session_id: String,
}

#[derive(Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct GetServersResult {
    #[serde(with = "serde_from_str")]
    index: i32,
    servers: Vec<Server>,
}

#[derive(Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Server {
    uri: String,
    current_uri: String,
    #[serde(with = "serde_from_str")]
    download_speed: u64,
}
