// https://specifications.freedesktop.org/idle-inhibit-spec/latest
// https://invent.kde.org/plasma/kscreenlocker/-/blob/master/dbus/org.freedesktop.ScreenSaver.xml

use futures_lite::StreamExt;
use logind_zbus::{manager::ManagerProxy, session::SessionProxy};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicU32, Ordering},
    },
    time::Instant,
};
use zbus::object_server::SignalEmitter;

use crate::{Event, EventSender};

// Clients vary in which path they use
const PATHS: [&str; 2] = ["/ScreenSaver", "/org/freedesktop/ScreenSaver"];

#[derive(Debug)]
pub struct Inhibitor {
    cookie: u32,
    application_name: String,
    reason_for_inhibit: String,
    client: zbus::names::UniqueName<'static>,
}

#[derive(Clone)]
struct Screensaver {
    inhibitors: Arc<Mutex<Vec<Inhibitor>>>,
    last_cookie: Arc<AtomicU32>,
    event_sender: EventSender,
    // When the session was locked, while it is locked
    locked_since: Arc<Mutex<Option<Instant>>>,
}

impl Screensaver {
    fn new(event_sender: EventSender) -> Self {
        Self {
            inhibitors: Arc::new(Mutex::new(Vec::new())),
            last_cookie: Arc::new(AtomicU32::new(0)),
            event_sender,
            locked_since: Arc::new(Mutex::new(None)),
        }
    }
}

#[zbus::interface(name = "org.freedesktop.ScreenSaver")]
impl Screensaver {
    fn inhibit(
        &mut self,
        application_name: String,
        reason_for_inhibit: String,
        #[zbus(header)] header: zbus::message::Header<'_>,
    ) -> u32 {
        let cookie = self.last_cookie.fetch_add(1, Ordering::Relaxed) + 1;
        if let Some(sender) = header.sender() {
            log::info!(
                "Added screensaver inhibitor for application '{}' {:?}, reason: {}, cookie: {}",
                application_name,
                sender,
                reason_for_inhibit,
                cookie
            );
            let mut inhibitors = self.inhibitors.lock().unwrap();
            if inhibitors.is_empty() {
                let _ = self.event_sender.send(Event::ScreensaverInhibit(true));
            }
            inhibitors.push(Inhibitor {
                cookie,
                application_name,
                reason_for_inhibit,
                client: sender.to_owned(),
            });
        }
        cookie
    }

    fn un_inhibit(&mut self, cookie: u32) {
        let mut inhibitors = self.inhibitors.lock().unwrap();
        if let Some(idx) = inhibitors.iter().position(|x| x.cookie == cookie) {
            let inhibitor = inhibitors.remove(idx);
            if inhibitors.is_empty() {
                let _ = self.event_sender.send(Event::ScreensaverInhibit(false));
            }
            log::info!(
                "Removed screensaver inhibitor for application '{}' {:?}, reason: {}, cookie: {}",
                inhibitor.application_name,
                inhibitor.client,
                inhibitor.reason_for_inhibit,
                inhibitor.cookie
            );
        }
    }

    /// Whether the session is locked.
    fn get_active(&self) -> bool {
        self.locked_since.lock().unwrap().is_some()
    }

    /// Seconds since the session was locked, or 0 while it is unlocked.
    fn get_active_time(&self) -> u32 {
        self.locked_since.lock().unwrap().map_or(0, |since| {
            u32::try_from(since.elapsed().as_secs()).unwrap_or(u32::MAX)
        })
    }

    #[zbus(signal)]
    async fn active_changed(emitter: &SignalEmitter<'_>, active: bool) -> zbus::Result<()>;
}

fn serve_at<'a>(
    builder: zbus::connection::Builder<'a>,
    screensaver: &Screensaver,
) -> zbus::Result<zbus::connection::Builder<'a>> {
    PATHS.iter().try_fold(builder, |builder, path| {
        builder.serve_at(*path, screensaver.clone())
    })
}

// Records the lock state, and announces a change on every path.
async fn set_active(
    conn: &zbus::Connection,
    locked_since: &Mutex<Option<Instant>>,
    active: bool,
) -> zbus::Result<()> {
    {
        let mut locked_since = locked_since.lock().unwrap();
        if locked_since.is_some() == active {
            return Ok(());
        }
        *locked_since = active.then(Instant::now);
    }
    log::info!("Session {}", if active { "locked" } else { "unlocked" });
    for path in PATHS {
        Screensaver::active_changed(&SignalEmitter::new(conn, path)?, active).await?;
    }
    Ok(())
}

// The screen locker sets the logind session's `LockedHint` while the session is locked.
async fn follow_session_lock(
    conn: &zbus::Connection,
    system: &zbus::Connection,
    locked_since: &Mutex<Option<Instant>>,
) -> zbus::Result<()> {
    let session_path = ManagerProxy::new(system).await?.get_session("auto").await?;
    let session = SessionProxy::builder(system)
        .path(session_path)?
        .build()
        .await?;
    // Yields the current value first
    let mut locked_hint = session.receive_locked_hint_changed().await;
    while let Some(change) = locked_hint.next().await {
        set_active(conn, locked_since, change.get().await?).await?;
    }
    Ok(())
}

// If a client disconnects from DBus, remove any inhibitors it has added.
async fn remove_inhibitors_of_closed_clients(
    conn: &zbus::Connection,
    inhibitors: &Mutex<Vec<Inhibitor>>,
    event_sender: &EventSender,
) -> zbus::Result<()> {
    let dbus = zbus::fdo::DBusProxy::new(conn).await?;
    let mut name_owner_stream = dbus.receive_name_owner_changed().await?;
    while let Some(event) = name_owner_stream.next().await {
        let args = event.args()?;
        if args.new_owner.is_none()
            && let zbus::names::BusName::Unique(name) = args.name
        {
            let mut inhibitors = inhibitors.lock().unwrap();
            if !inhibitors.is_empty() {
                inhibitors.retain(|inhibitor| inhibitor.client != name);
                if inhibitors.is_empty() {
                    let _ = event_sender.send(Event::ScreensaverInhibit(false));
                }
            }
        }
    }
    Ok(())
}

pub async fn serve(event_sender: EventSender) -> zbus::Result<()> {
    let screensaver = Screensaver::new(event_sender.clone());

    let conn = serve_at(zbus::connection::Builder::session()?, &screensaver)?
        .build()
        .await?;
    conn.request_name_with_flags(
        "org.freedesktop.ScreenSaver",
        zbus::fdo::RequestNameFlags::ReplaceExisting.into(),
    )
    .await?;

    let follow_lock = async {
        let system = zbus::Connection::system().await?;
        follow_session_lock(&conn, &system, &screensaver.locked_since).await
    };
    let (follow_lock, cleanup) = futures_lite::future::zip(
        follow_lock,
        remove_inhibitors_of_closed_clients(&conn, &screensaver.inhibitors, &event_sender),
    )
    .await;
    if let Err(err) = follow_lock {
        log::error!("failed to follow the session's lock state: {err}");
    }
    cleanup
}

#[cfg(test)]
mod tests {
    use super::*;
    use calloop::channel;
    use futures_lite::future;
    use std::{os::unix::net::UnixStream, time::Duration};
    use zbus::zvariant::OwnedObjectPath;

    const SESSION_PATH: &str = "/org/freedesktop/login1/session/_31";

    struct FakeManager;

    #[zbus::interface(name = "org.freedesktop.login1.Manager")]
    impl FakeManager {
        fn get_session(&self, session_id: &str) -> OwnedObjectPath {
            assert_eq!(session_id, "auto");
            OwnedObjectPath::try_from(SESSION_PATH).unwrap()
        }
    }

    struct FakeSession {
        locked_hint: bool,
    }

    #[zbus::interface(name = "org.freedesktop.login1.Session")]
    impl FakeSession {
        #[zbus(property)]
        fn locked_hint(&self) -> bool {
            self.locked_hint
        }
    }

    #[zbus::proxy(
        interface = "org.freedesktop.ScreenSaver",
        default_service = "org.freedesktop.ScreenSaver"
    )]
    trait App {
        fn get_active(&self) -> zbus::Result<bool>;
        fn get_active_time(&self) -> zbus::Result<u32>;
        #[zbus(signal)]
        fn active_changed(&self, active: bool) -> zbus::Result<()>;
    }

    // A connected pair of peer-to-peer connections; the first serves `objects`.
    async fn peers(
        objects: impl FnOnce(
            zbus::connection::Builder<'static>,
        ) -> zbus::Result<zbus::connection::Builder<'static>>,
    ) -> (zbus::Connection, zbus::Connection) {
        let (server, client) = UnixStream::pair().unwrap();
        let server = objects(
            zbus::connection::Builder::async_io_unix_stream(server)
                .server(zbus::Guid::generate())
                .unwrap()
                .p2p(),
        )
        .unwrap()
        .build();
        let client = zbus::connection::Builder::async_io_unix_stream(client)
            .p2p()
            .build();
        let (server, client) = future::zip(server, client).await;
        (server.unwrap(), client.unwrap())
    }

    async fn logind(locked_hint: bool) -> (zbus::Connection, zbus::Connection) {
        peers(|builder| {
            builder
                .serve_at("/org/freedesktop/login1", FakeManager)?
                .serve_at(SESSION_PATH, FakeSession { locked_hint })
        })
        .await
    }

    async fn set_locked_hint(logind: &zbus::Connection, locked_hint: bool) {
        let session = logind
            .object_server()
            .interface::<_, FakeSession>(SESSION_PATH)
            .await
            .unwrap();
        session.get_mut().await.locked_hint = locked_hint;
        session
            .get()
            .await
            .locked_hint_changed(session.signal_emitter())
            .await
            .unwrap();
    }

    async fn screensaver() -> (Screensaver, zbus::Connection, zbus::Connection) {
        let (event_sender, _) = channel::channel();
        let screensaver = Screensaver::new(event_sender);
        let (idle, app) = peers(|builder| serve_at(builder, &screensaver)).await;
        (screensaver, idle, app)
    }

    async fn app_at(app: &zbus::Connection, path: &'static str) -> AppProxy<'static> {
        AppProxy::builder(app)
            .path(path)
            .unwrap()
            .build()
            .await
            .unwrap()
    }

    // Fails the test instead of hanging when an expected signal never comes.
    async fn within_a_second<T>(task: impl Future<Output = T>) -> T {
        future::or(task, async {
            async_io::Timer::after(Duration::from_secs(1)).await;
            panic!("timed out")
        })
        .await
    }

    #[test]
    fn active_follows_the_session_lock_on_every_path() {
        future::block_on(async {
            let (logind, system) = logind(false).await;
            let (screensaver, idle, app) = screensaver().await;
            let apps = [app_at(&app, PATHS[0]).await, app_at(&app, PATHS[1]).await];
            let mut streams = Vec::new();
            for app in &apps {
                streams.push(app.receive_active_changed().await.unwrap());
            }

            let follow = follow_session_lock(&idle, &system, &screensaver.locked_since);
            let checks = async {
                for app in &apps {
                    assert!(!app.get_active().await.unwrap());
                    assert_eq!(app.get_active_time().await.unwrap(), 0);
                }

                for locked in [true, false, true] {
                    set_locked_hint(&logind, locked).await;
                    for stream in &mut streams {
                        let changed = within_a_second(stream.next()).await.unwrap();
                        assert_eq!(changed.args().unwrap().active, locked);
                    }
                    for app in &apps {
                        assert_eq!(app.get_active().await.unwrap(), locked);
                        assert_eq!(app.get_active_time().await.unwrap(), 0);
                    }
                }
            };
            future::or(async { panic!("stopped: {:?}", follow.await) }, checks).await;
        });
    }

    #[test]
    fn starts_active_when_the_session_is_already_locked() {
        future::block_on(async {
            let (_logind, system) = logind(true).await;
            let (screensaver, idle, app) = screensaver().await;
            let app = app_at(&app, PATHS[1]).await;
            let mut stream = app.receive_active_changed().await.unwrap();

            let follow = follow_session_lock(&idle, &system, &screensaver.locked_since);
            let checks = async {
                let changed = within_a_second(stream.next()).await.unwrap();
                assert!(changed.args().unwrap().active);
                assert!(app.get_active().await.unwrap());
            };
            future::or(async { panic!("stopped: {:?}", follow.await) }, checks).await;
        });
    }

    #[test]
    fn active_time_counts_seconds_since_the_lock() {
        let (event_sender, _) = channel::channel();
        let screensaver = Screensaver::new(event_sender);
        assert_eq!(screensaver.get_active_time(), 0);

        *screensaver.locked_since.lock().unwrap() =
            Instant::now().checked_sub(Duration::from_secs(90));
        assert_eq!(screensaver.get_active_time(), 90);
    }
}
