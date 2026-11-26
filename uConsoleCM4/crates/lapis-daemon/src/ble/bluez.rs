//! BlueZ GATT peripheral for lapis-ble-v1 (via `bluer`). Thin on purpose: every protocol
//! decision lives in `session.rs`. No link-layer pairing flags: the protocol brings its own
//! authenticated encryption bound to the on-screen PIN, and bonding would only add an OS
//! dialog on the phone (as on Lapis-Zero).

use super::session::{Outbound, Session};
use super::{CMD_TX_UUID, EVT_RX_UUID, HANDSHAKE_UUID, NAME_PREFIX, SERVICE_UUID};
use anyhow::Context;
use bluer::adv::Advertisement;
use bluer::gatt::local::{
    Application, Characteristic, CharacteristicNotifier, CharacteristicNotify,
    CharacteristicNotifyMethod, CharacteristicRead, CharacteristicWrite, CharacteristicWriteMethod,
    ReqError, Service,
};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, Mutex};

const NOTIFY_GAP: Duration = Duration::from_millis(20);

fn uuid(s: &str) -> anyhow::Result<bluer::Uuid> {
    s.parse().context("bad UUID")
}

/// Advertises for `window`, serves the session until it finishes or the window (extended
/// to `finish` once the PIN is verified) runs out. Returns the advertised name.
pub async fn run_window(
    session: Arc<Session>,
    mut out: mpsc::UnboundedReceiver<Outbound>,
    window: Duration,
    finish: Duration,
    on_name: impl Fn(&str),
) -> anyhow::Result<()> {
    let bt = bluer::Session::new().await.context("BlueZ D-Bus")?;
    let adapter = bt.default_adapter().await.context("no Bluetooth adapter")?;
    adapter
        .set_powered(true)
        .await
        .context("power on adapter")?;
    let addr = adapter.address().await?.to_string().replace(':', "");
    let name = format!("{NAME_PREFIX}{}", &addr[addr.len().saturating_sub(4)..]);
    adapter.set_alias(name.clone()).await?;
    on_name(&name);

    let adv = adapter
        .advertise(Advertisement {
            service_uuids: [uuid(SERVICE_UUID)?].into_iter().collect(),
            discoverable: Some(true),
            local_name: Some(name.clone()),
            ..Default::default()
        })
        .await
        .context("register advertisement")?;

    let notifier: Arc<Mutex<Option<CharacteristicNotifier>>> = Arc::new(Mutex::new(None));
    let peer: Arc<Mutex<Option<bluer::Address>>> = Arc::new(Mutex::new(None));
    let (inbox_tx, mut inbox) = mpsc::channel::<Vec<u8>>(256);

    let (s_read, s_write) = (session.clone(), session.clone());
    let (p_write, p_cmd) = (peer.clone(), peer.clone());
    let s_cmd = session.clone();
    let n_slot = notifier.clone();
    let app = Application {
        services: vec![Service {
            uuid: uuid(SERVICE_UUID)?,
            primary: true,
            characteristics: vec![
                Characteristic {
                    uuid: uuid(HANDSHAKE_UUID)?,
                    read: Some(CharacteristicRead {
                        read: true,
                        fun: Box::new(move |req| {
                            let s = s_read.clone();
                            Box::pin(async move {
                                s.set_mtu(usize::from(req.mtu)).await;
                                let hello = s.handshake_read().await;
                                Ok(hello
                                    .get(usize::from(req.offset)..)
                                    .map(<[u8]>::to_vec)
                                    .unwrap_or_default())
                            })
                        }),
                        ..Default::default()
                    }),
                    write: Some(CharacteristicWrite {
                        write: true,
                        method: CharacteristicWriteMethod::Fun(Box::new(move |value, req| {
                            let (s, p) = (s_write.clone(), p_write.clone());
                            Box::pin(async move {
                                s.set_mtu(usize::from(req.mtu)).await;
                                *p.lock().await = Some(req.device_address);
                                s.handshake_write(&value)
                                    .await
                                    .map_err(|_| ReqError::Failed)
                            })
                        })),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                Characteristic {
                    uuid: uuid(CMD_TX_UUID)?,
                    write: Some(CharacteristicWrite {
                        write: true,
                        write_without_response: true,
                        method: CharacteristicWriteMethod::Fun(Box::new(move |value, req| {
                            let (s, p, tx) = (s_cmd.clone(), p_cmd.clone(), inbox_tx.clone());
                            Box::pin(async move {
                                s.set_mtu(usize::from(req.mtu)).await;
                                *p.lock().await = Some(req.device_address);
                                tx.try_send(value).map_err(|_| ReqError::Failed)
                            })
                        })),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                Characteristic {
                    uuid: uuid(EVT_RX_UUID)?,
                    notify: Some(CharacteristicNotify {
                        notify: true,
                        method: CharacteristicNotifyMethod::Fun(Box::new(move |n| {
                            let slot = n_slot.clone();
                            Box::pin(async move {
                                *slot.lock().await = Some(n);
                            })
                        })),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
            ],
            ..Default::default()
        }],
        ..Default::default()
    };
    let _app = adapter
        .serve_gatt_application(app)
        .await
        .context("register GATT application")?;
    tracing::info!(%name, window_s = window.as_secs(), "BLE provisioning: advertising");

    let started = tokio::time::Instant::now();
    let mut adv = Some(adv);
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    loop {
        tokio::select! {
            Some(frame) = inbox.recv() => session.on_frame(&frame).await,
            Some(o) = out.recv() => match o {
                Outbound::Frame(f) => {
                    if let Some(n) = notifier.lock().await.as_mut() {
                        if n.notify(f).await.is_err() {
                            tracing::warn!("BLE notify failed (phone gone?)");
                        }
                    }
                    tokio::time::sleep(NOTIFY_GAP).await;
                }
                Outbound::Disconnect => {
                    if let Some(addr) = peer.lock().await.take() {
                        if let Ok(dev) = adapter.device(addr) {
                            let _ = dev.disconnect().await;
                        }
                    }
                }
            },
            _ = tick.tick() => {
                if session.is_done().await {
                    break;
                }
                let limit = if session.is_verified().await { window.max(finish) } else { window };
                if started.elapsed() > limit {
                    tracing::info!("BLE provisioning window closed");
                    break;
                }
                if started.elapsed() > window && adv.is_some() {
                    adv = None; // stop advertising; a verified phone may still finish
                }
            }
        }
    }
    drop(adv);
    session.close();
    Ok(())
}
