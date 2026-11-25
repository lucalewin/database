use std::sync::Arc;

use tokio::{
    runtime::Handle,
    sync::{mpsc, oneshot},
};

use super::postgres::save_last_connection;

type SaveResult = Result<(), String>;
enum SaveRequest {
    Save(String, oneshot::Sender<SaveResult>),
    Flush(oneshot::Sender<()>),
}

/// Serializes committed selections so an older write cannot finish last.
#[derive(Clone)]
pub(crate) struct LastConnectionWriter {
    sender: mpsc::UnboundedSender<SaveRequest>,
}

impl LastConnectionWriter {
    pub(crate) fn new(runtime: &Handle) -> Self {
        Self::with_writer(runtime, save_last_connection)
    }

    fn with_writer(
        runtime: &Handle,
        write: impl Fn(&str) -> SaveResult + Send + Sync + 'static,
    ) -> Self {
        let (sender, mut receiver) = mpsc::unbounded_channel::<SaveRequest>();
        let write = Arc::new(write);
        let runtime = runtime.clone();
        runtime.clone().spawn(async move {
            while let Some(request) = receiver.recv().await {
                match request {
                    SaveRequest::Save(id, response) => {
                        let write = write.clone();
                        let result = runtime
                            .spawn_blocking(move || write(&id))
                            .await
                            .unwrap_or_else(|_| Err("Could not save the last connection".into()));
                        let _ = response.send(result);
                    }
                    SaveRequest::Flush(response) => {
                        let _ = response.send(());
                    }
                }
            }
        });
        Self { sender }
    }

    pub(crate) fn enqueue(&self, id: String) -> oneshot::Receiver<SaveResult> {
        let (response, receiver) = oneshot::channel();
        if let Err(mpsc::error::SendError(SaveRequest::Save(_, response))) =
            self.sender.send(SaveRequest::Save(id, response))
        {
            let _ = response.send(Err("Could not save the last connection".into()));
        }
        receiver
    }

    /// Waits for earlier writes without blocking the UI thread during shutdown.
    pub(crate) fn flush(&self) -> oneshot::Receiver<()> {
        let (response, receiver) = oneshot::channel();
        let _ = self.sender.send(SaveRequest::Flush(response));
        receiver
    }
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{Mutex, mpsc as std_mpsc},
        time::Duration,
    };

    use super::*;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn writes_are_serialized_even_when_the_first_write_is_slow() {
        let (started_tx, started_rx) = std_mpsc::channel();
        let (release_tx, release_rx) = std_mpsc::channel();
        let release_rx = Mutex::new(release_rx);
        let writer = LastConnectionWriter::with_writer(&Handle::current(), move |id| {
            started_tx.send(id.to_owned()).unwrap();
            if id == "first" {
                release_rx.lock().unwrap().recv().unwrap();
            }
            Ok(())
        });

        let first = writer.enqueue("first".into());
        let second = writer.enqueue("second".into());
        let mut flushed = writer.flush();
        assert_eq!(
            started_rx.recv_timeout(Duration::from_secs(2)).unwrap(),
            "first"
        );
        assert!(started_rx.try_recv().is_err());
        assert!(matches!(
            flushed.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        ));
        release_tx.send(()).unwrap();
        assert!(first.await.unwrap().is_ok());
        assert!(second.await.unwrap().is_ok());
        flushed.await.unwrap();
        assert_eq!(
            started_rx.recv_timeout(Duration::from_secs(2)).unwrap(),
            "second"
        );
    }

    #[tokio::test]
    async fn failed_write_does_not_prevent_later_writes() {
        let writer = LastConnectionWriter::with_writer(&Handle::current(), |id| {
            if id == "first" {
                Err("write failed".into())
            } else {
                Ok(())
            }
        });
        let first = writer.enqueue("first".into());
        let second = writer.enqueue("second".into());
        assert_eq!(first.await.unwrap(), Err("write failed".into()));
        assert_eq!(second.await.unwrap(), Ok(()));
    }
}
