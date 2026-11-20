
use std::{
    future::{self, Future},
    sync::{OnceLock, mpsc},
    thread,
    time::Duration,
};

use tokio::{
    runtime::{Builder, Handle},
    sync::OnceCell,
};
use zbus::Connection;

const TIMEOUT: Duration = Duration::from_secs(2);

fn runtime() -> Option<&'static Handle> {
    static RUNTIME: OnceLock<Option<Handle>> = OnceLock::new();
    RUNTIME
        .get_or_init(|| {
            let runtime = Builder::new_current_thread()
                .enable_all()
                .build()
                .inspect_err(|err| tracing::warn!("cannot start the D-Bus thread: {err}"))
                .ok()?;
            let handle = runtime.handle().clone();
            thread::Builder::new()
                .name("tunebox-dbus".into())
                .spawn(move || runtime.block_on(future::pending::<()>()))
                .inspect_err(|err| tracing::warn!("cannot start the D-Bus thread: {err}"))
                .ok()?;
            Some(handle)
        })
        .as_ref()
}

pub fn spawn(task: impl Future<Output = ()> + Send + 'static) {
    if let Some(runtime) = runtime() {
        runtime.spawn(task);
    }
}

pub fn run<T: Send + 'static>(task: impl Future<Output = Option<T>> + Send + 'static) -> Option<T> {
    let (tx, rx) = mpsc::sync_channel(1);
    spawn(async move {
        let _ = tx.send(task.await);
    });
    rx.recv_timeout(TIMEOUT).ok().flatten()
}

pub async fn session() -> Option<Connection> {
    static SESSION: OnceCell<Option<Connection>> = OnceCell::const_new();
    SESSION
        .get_or_init(|| async {
            Connection::session().await.inspect_err(|err| tracing::debug!("no session bus: {err}")).ok()
        })
        .await
        .clone()
}

pub async fn system() -> Option<Connection> {
    static SYSTEM: OnceCell<Option<Connection>> = OnceCell::const_new();
    SYSTEM
        .get_or_init(|| async {
            Connection::system().await.inspect_err(|err| tracing::debug!("no system bus: {err}")).ok()
        })
        .await
        .clone()
}
