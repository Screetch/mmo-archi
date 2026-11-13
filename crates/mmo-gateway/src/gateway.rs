//! What each client of the gateway shares: Redis, the endpoint, and the link to the writer of each
//! layer that the directory names.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use mmo_protocol::{AccountId, CharacterId, Layer, ShardId, Token, ZoneId};
use mmo_store::{Directory, Serving, keys};
use quinn::{Connection, Endpoint};
use redis::aio::{ConnectionManager, ConnectionManagerConfig};
use redis::{AsyncCommands, PushInfo, RedisError};
use tokio::net::lookup_host;
use tokio::sync::Mutex;
use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};
use tokio::time;

use crate::link::Fault;
use crate::writer::{self, Writer};

/// The time after which the gateway reads the directory again, when no message on its channel
/// came before.
const REFRESH: Duration = Duration::from_secs(5);

/// The stores and the links that the clients of the gateway share.
pub(crate) struct Gateway {
  redis: ConnectionManager,
  endpoint: Endpoint,
  /// The family of the address of the endpoint, which the address of a layer writer must share.
  ipv4: bool,
  /// The link to the writer of each layer, by its shard, its zone and its number. A client holds
  /// the lock while it opens a link, so one gateway never opens two links to one writer.
  writers: Mutex<BTreeMap<(u32, i32, u32), Writer>>,
}

/// The session of a client, which the login service wrote. A player never changes its shard in a
/// session.
pub(crate) struct Session {
  pub(crate) account: AccountId,
  pub(crate) character: CharacterId,
  pub(crate) shard: ShardId,
  pub(crate) layer: Layer,
}

/// The account, the character, the shard, the zone and the layer of a session in Redis.
type SessionFields = (Option<i64>, Option<i64>, Option<u32>, Option<i32>, Option<u32>);

fn key(shard: ShardId, layer: Layer) -> (u32, i32, u32) {
  (shard.0, layer.zone.0, layer.number)
}

impl Gateway {
  /// Connects to Redis with `redis`, which speaks RESP3, hears the channel of the directory, and
  /// reads again the lease of each linked layer at each message on it, and each 5 s.
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
    let fields = [keys::ACCOUNT, keys::CHARACTER, keys::SHARD, keys::ZONE, keys::LAYER];
    let found: SessionFields = self.redis.clone().hmget(keys::session(token), &fields).await?;
    Ok(match found {
      (Some(account), Some(character), Some(shard), Some(zone), Some(number)) => Some(Session {
        account: AccountId(account),
        character: CharacterId(character),
        shard: ShardId(shard),
        layer: Layer { zone: ZoneId(zone), number },
      }),
      _ => None,
    })
  }

  /// The link to the writer of `layer` of `shard`, which the gateway opens when it holds none.
  pub(crate) async fn writer(
    self: &Arc<Self>,
    shard: ShardId,
    layer: Layer,
  ) -> Result<Writer, Fault> {
    let mut writers = self.writers.lock().await;
    if let Some(held) = writers.get(&key(shard, layer)) {
      return Ok(held.clone());
    }
    let entry = self.entry(shard, layer).await?.ok_or(Fault::NoWriter(layer))?;
    let connection = self.connect(&entry.address).await?;
    let opened = writer::open(Arc::clone(self), (shard, layer), entry, connection).await?;
    writers.insert(key(shard, layer), opened.clone());
    Ok(opened)
  }

  /// Forgets the link to the writer of `layer` of `shard` when it is the link `id` that ended, so
  /// the next client of the layer opens another.
  pub(crate) async fn forget(&self, shard: ShardId, layer: Layer, id: usize) {
    let mut writers = self.writers.lock().await;
    if writers.get(&key(shard, layer)).is_some_and(|held| held.id() == id) {
      writers.remove(&key(shard, layer));
    }
  }

  /// Opens a link to the layer writer at `address`, as `host:port`. The name of the host is the
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

  /// Reads again the lease of each linked layer, and ends each link whose layer has another
  /// writer now, or none. Its clients end with it, attach again, and the first of them opens the
  /// link to the new writer.
  async fn refresh(&self) {
    let mut writers = self.writers.lock().await;
    let mut read = BTreeMap::new();
    for (key, held) in writers.iter() {
      read.insert(*key, self.entry(held.shard, held.layer).await);
    }
    writers.retain(|key, held| match &read[key] {
      Ok(entry) if entry.as_ref() == Some(held.entry()) => true,
      Ok(_) => {
        held.end();
        false
      }
      Err(fault) => {
        let (shard, zone, number) = key;
        eprintln!(
          "the gateway cannot read the lease of layer {zone}:{number} of shard {shard}, and keeps \
           its link: {fault}"
        );
        true
      }
    });
  }

  /// The lease of `layer` of `shard` in the directory, or `None` when no writer holds the layer,
  /// or when its writer waits for the epoch of the keeper and does not serve it yet.
  async fn entry(&self, shard: ShardId, layer: Layer) -> Result<Option<Serving>, RedisError> {
    Directory { shard }.serving(&mut self.redis.clone(), layer).await
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
