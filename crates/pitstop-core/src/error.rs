use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("not logged in to {0} — sign in with the tool first, then enroll")]
    NotLoggedIn(String),
    #[error(
        "account `{0}` already exists for {1}; pick another name or `piaz-switch remove {0}` first"
    )]
    DuplicateAccount(String, String),
    #[error(
        "this {provider} login is already enrolled as `{name}`; to add another account, sign in to it with `piaz-switch enroll <name> --sign-in`"
    )]
    AlreadyEnrolled { name: String, provider: String },
    #[error("that sign-in was {}, not `{name}`; sign in with `{name}`'s account", label.as_deref().unwrap_or("another account"))]
    WrongAccount { name: String, label: Option<String> },
    #[error("no account named `{0}` — see `piaz-switch list`")]
    UnknownAccount(String),
    #[error("account name `{0}` is ambiguous across providers; pass --provider")]
    AmbiguousAccount(String),
    #[error("invalid account name `{0}`: use letters, digits, `-` and `_` (max 64 chars)")]
    InvalidName(String),
    #[error("vault key is missing from the OS keystore; parked accounts must be re-enrolled")]
    VaultKeyMissing,
    #[error("no parked credential for `{0}`")]
    VaultBlobMissing(String),
    #[error("vault data for `{0}` is corrupt or cannot be decrypted")]
    VaultCorrupt(String),
    #[error("io error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("{0} was not found on this computer; install it, or put it on your PATH")]
    ToolNotFound(String),
    #[error("{0}")]
    SignIn(String),
    #[error("{0} is still open; close it, then try again")]
    EditorStillOpen(String),
    #[error("keystore error: {0}")]
    Keystore(String),
    #[error("invalid data: {0}")]
    InvalidData(String),
}

pub type Result<T> = std::result::Result<T, Error>;
