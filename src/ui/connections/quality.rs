use std::time::{Duration, Instant};

use gpui_kit::{AsyncApp, Context, Task};
use tokio::runtime::Handle;
use uuid::Uuid;

use crate::connection::manager::ManagedConnection;

const PROBE_INTERVAL: Duration = Duration::from_secs(30);
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnectionQuality {
    Disconnected,
    Checking,
    Reachable(Duration),
    Unavailable,
}

impl ConnectionQuality {
    pub fn label(self) -> String {
        match self {
            Self::Disconnected => "Connection: disconnected".into(),
            Self::Checking => "Connection: checking…".into(),
            Self::Unavailable => "Connection: unavailable".into(),
            Self::Reachable(latency) => {
                let quality = if latency < Duration::from_millis(100) {
                    "Good"
                } else if latency < Duration::from_millis(500) {
                    "Fair"
                } else {
                    "Slow"
                };
                format!("Connection: {quality} · {} ms", latency.as_millis())
            }
        }
    }
}

pub struct ConnectionMonitor {
    runtime: Handle,
    connection_id: Option<Uuid>,
    quality: ConnectionQuality,
    task: Option<Task<()>>,
}

impl ConnectionMonitor {
    pub fn new(runtime: Handle) -> Self {
        Self {
            runtime,
            connection_id: None,
            quality: ConnectionQuality::Disconnected,
            task: None,
        }
    }

    pub fn quality(&self) -> ConnectionQuality {
        self.quality
    }

    pub(crate) fn set_connection(
        &mut self,
        connection: Option<ManagedConnection>,
        cx: &mut Context<Self>,
    ) {
        let connection_id = connection.as_ref().map(ManagedConnection::id);
        if self.connection_id == connection_id {
            return;
        }
        self.task = None;
        self.connection_id = connection_id;
        let Some(connection) = connection else {
            self.quality = ConnectionQuality::Disconnected;
            cx.notify();
            return;
        };
        self.quality = ConnectionQuality::Checking;
        cx.notify();
        let runtime = self.runtime.clone();
        self.task = Some(cx.spawn(async move |this, cx: &mut AsyncApp| {
            loop {
                let connection = connection.clone();
                let mut probe_task = ProbeTask(runtime.spawn(probe(connection)));
                let result = (&mut probe_task.0)
                    .await
                    .unwrap_or(ConnectionQuality::Unavailable);
                if this
                    .update(cx, |state, cx| {
                        state.quality = result;
                        cx.notify();
                    })
                    .is_err()
                {
                    break;
                }
                cx.background_executor().timer(PROBE_INTERVAL).await;
            }
        }));
    }
}

// Dropping the GPUI task on a connection change must also cancel its Tokio query.
struct ProbeTask(tokio::task::JoinHandle<ConnectionQuality>);

impl Drop for ProbeTask {
    fn drop(&mut self) {
        self.0.abort();
    }
}

async fn probe(connection: ManagedConnection) -> ConnectionQuality {
    let start = Instant::now();
    match tokio::time::timeout(PROBE_TIMEOUT, connection.ping()).await {
        Ok(Ok(_)) => ConnectionQuality::Reachable(start.elapsed()),
        _ => ConnectionQuality::Unavailable,
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use crate::ui::connections::quality::{ConnectionQuality, probe};

    #[tokio::test]
    async fn a_closed_pool_is_unavailable_without_exposing_an_error() {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy_with(sqlx::postgres::PgConnectOptions::new());
        pool.close().await;
        let connection = crate::database::postgres::test_connection(
            "local",
            "db",
            crate::connection::ConnectionTier::Local,
            pool,
        );
        assert_eq!(probe(connection).await, ConnectionQuality::Unavailable);
    }

    #[test]
    fn quality_labels_describe_latency_and_failure() {
        assert_eq!(
            ConnectionQuality::Disconnected.label(),
            "Connection: disconnected"
        );
        assert_eq!(ConnectionQuality::Checking.label(), "Connection: checking…");
        assert_eq!(
            ConnectionQuality::Reachable(Duration::from_millis(99)).label(),
            "Connection: Good · 99 ms"
        );
        assert_eq!(
            ConnectionQuality::Reachable(Duration::from_millis(100)).label(),
            "Connection: Fair · 100 ms"
        );
        assert_eq!(
            ConnectionQuality::Reachable(Duration::from_millis(500)).label(),
            "Connection: Slow · 500 ms"
        );
        assert_eq!(
            ConnectionQuality::Unavailable.label(),
            "Connection: unavailable"
        );
    }
}
