//! What each client of the gateway shares: Redis, the endpoint, and the link to each zone writer
//! that the directory names.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use mmo_protocol::{AccountId, CharacterId, Token, ZoneId};
use mmo_store::keys;
use quinn::{Connection, Endpoint};
use redis::aio::{ConnectionManager, ConnectionManagerConfig};
use redis::{AsyncCommands, PushInfo, RedisError};
use tokio::net::lookup_host;
use tokio::sync::Mutex;
use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};
use tokio::time;

use crate::link::Fault;
use crate::writer::{self, Entry, Writer};

/// The time after which the gateway reads the directory again, when no message on its channel
/// came before.
const REFRESH: Duration = Duration::from_secs(5);

/// The stores and the links that the clients of the gateway share.
pub(crate) struct Gateway {
  redis: ConnectionManager,
  endpoint: Endpoint,
  /// The family of the address of the endpoint, which the address of a zone writer must share.
  ipv4: bool,
  /// The link to the writer of each zone, by the id of the zone. A client holds the lock while it
  /// opens a link, so one gateway never opens two links to one writer.
  writers: Mutex<BTreeMap<i32, Writer>>,
}

/// The session of a client, which the login service wrote.
pub(crate) struct Session {
  pub(crate) account: AccountId,
  pub(crate) character: CharacterId,
  pub(crate) zone: ZoneId,
}

impl Gateway {
  /// Connects to Redis with `redis`, which speaks RESP3, hears the channel of the directory, and
  /// reads again the entry of each linked zone at each message on it, and each 5 s.
  pub(crate) async fn open(
    redis: redis::Client,
    endpoint: Endpoint,
    ipv4: bool,
  ) -> Result<Arc<Self>, RedisError> {
    let (pushes, heard) = unbounded_channel();
    let config =
      ConnectionManagerConfig::new().set_push_sender(pushes).set_automatic_resubscription();
    let mut redis = ConnectionManager::new_with_config(redis, config).await?;
    redis.subscribe(keys::DIRECTORY).await?;
    let gateway = Arc::new(Self { redis, endpoint, ipv4, writers: Mutex::default() });
    tokio::spawn(watch(Arc::clone(&gateway), heard));
    Ok(gateway)
  }

  /// The session of `token`, or `None` when the login service gave none or it ended.
  pub(crate) async fn session(&self, token: &Token) -> Result<Option<Session>, RedisError> {
    let fields = ["account", "character", "zone"];
    let found: (Option<i64>, Option<i64>, Option<i32>) =
      self.redis.clone().hmget(keys::session(token), &fields).await?;
    Ok(match found {
      (Some(account), Some(character), Some(zone)) => Some(Session {
        account: AccountId(account),
        character: CharacterId(character),
        zone: ZoneId(zone),
      }),
      _ => None,
    })
  }

  /// The link to the writer of `zone`, which the gateway opens when it holds none.
  pub(crate) async fn writer(self: &Arc<Self>, zone: ZoneId) -> Result<Writer, Fault> {
    let mut writers = self.writers.lock().await;
    if let Some(held) = writers.get(&zone.0) {
      return Ok(held.clone());
    }
    let entry = self.entry(zone).await?.ok_or(Fault::NoWriter(zone))?;
    let connection = self.connect(&entry.address).await?;
    let opened = writer::open(Arc::clone(self), zone, entry, connection).await?;
    writers.insert(zone.0, opened.clone());
    Ok(opened)
  }

  /// Forgets the link to the writer of `zone` when it is the link `id` that ended, so the next
  /// client of the zone opens another.
  pub(crate) async fn forget(&self, zone: ZoneId, id: usize) {
    let mut writers = self.writers.lock().await;
    if writers.get(&zone.0).is_some_and(|held| held.id() == id) {
      writers.remove(&zone.0);
    }
  }

  /// Opens a link to the zone writer at `address`, as `host:port`. The name of the host is the
  /// name that the certificate of the writer must hold.
  async fn connect(&self, address: &str) -> Result<Connection, Fault> {
    let host = address.rsplit_once(':').map_or(address, |(host, _)| host);
    let found = lookup_host(address)
      .await
      .map_err(Fault::Resolve)?
      .find(|found| found.is_ipv4() == self.ipv4)
      .ok_or_else(|| Fault::Unresolved(address.to_owned()))?;
    Ok(self.endpoint.connect(found, host.trim_matches(['[', ']']))?.await?)
  }

  /// Reads again the entry of each linked zone, and ends each link whose zone has another writer
  /// now, or none. Its clients end with it, attach again, and the first of them opens the link to
  /// the new writer.
  async fn refresh(&self) {
    let mut writers = self.writers.lock().await;
    let mut read = BTreeMap::new();
    for zone in writers.keys() {
      read.insert(*zone, self.entry(ZoneId(*zone)).await);
    }
    writers.retain(|zone, held| match &read[zone] {
      Ok(entry) if entry.as_ref() == Some(held.entry()) => true,
      Ok(_) => {
        held.end();
        false
      }
      Err(fault) => {
        eprintln!("the gateway cannot read the entry of zone {zone}, and keeps its link: {fault}");
        true
      }
    });
  }

  /// The entry of `zone` in the directory, or `None` when no writer holds the zone.
  async fn entry(&self, zone: ZoneId) -> Result<Option<Entry>, RedisError> {
    let found: (Option<String>, Option<i64>) =
      self.redis.clone().hmget(keys::zone(zone), &["address", "epoch"]).await?;
    Ok(found.0.zip(found.1).map(|(address, epoch)| Entry { address, epoch }))
  }
}

async fn watch(gateway: Arc<Gateway>, mut heard: UnboundedReceiver<PushInfo>) {
  let mut each = time::interval(REFRESH);
  loop {
    tokio::select! {
      _ = each.tick() => {}
      Some(_) = heard.recv() => {}
    }
    gateway.refresh().await;
  }
}
