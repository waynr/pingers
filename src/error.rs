
pub(crate) type Result<T> = std::result::Result<T, Error>;

#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("{0}")]
    GenericStringError(String),

    #[error("")]
    StdIoError(#[from] std::io::Error),

    #[error("")]
    CsvError(#[from] csv::Error),

    #[error("")]
    RtnetlinkError(#[from] rtnetlink::Error),

    #[error("")]
    AsyncChannelRecvError(#[from] async_channel::RecvError),

    #[error("")]
    TokioJoinError(#[from] tokio::task::JoinError),

    #[error("")]
    AsyncChannelTargetParamsSendError(#[from] async_channel::SendError<super::prober::TargetParams>),
}
