use crate::handle::RpcTaskHandleShared;
use async_trait::async_trait;
use mm2_err_handle::prelude::*;
use serde::Serialize;

pub trait RpcTaskTypes {
    type Item: Serialize + Clone + Send + Sync + 'static;
    type Error: SerMmErrorType + Clone + Send + Sync + 'static;
    type InProgressStatus: Serialize + Clone + Send + Sync + 'static;
    type AwaitingStatus: Serialize + Clone + Send + Sync + 'static;
    type UserAction: NotMmError + Send + Sync + 'static;
}

#[async_trait]
pub trait RpcTask: RpcTaskTypes + Sized + Send + 'static {
    fn initial_status(&self) -> Self::InProgressStatus;

    /// Returns the ID of the client that initiated/requesting the task.
    ///
    /// This is related to event streaming and is used to identify the client to whom the updates shall be sent.
    fn client_id(&self) -> Option<u64>;

    /// The method is invoked when the task has been cancelled.
    async fn cancel(self);

    async fn run(&mut self, task_handle: RpcTaskHandleShared<Self>) -> Result<Self::Item, MmError<Self::Error>>;
}
