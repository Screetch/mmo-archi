use bitcode::{Decode, Encode};

use crate::{
  AccountId, Angle, CharacterId, Epoch, Layer, Link, Offset, Password, Position, ShardId,
  TICKS_PER_SECOND, Tick, Token, Velocity, ZoneId,
};

/// A message of a client to the login service, on a stream.
#[derive(Clone, Debug, PartialEq, Eq, Encode, Decode)]
pub enum ToLogin {
  /// Makes an account, then enters the queue as [`ToLogin::Login`] does. The name holds 3 to 16
  /// letters and digits of ASCII, and the login service puts it in lower case. The password holds
  /// 8 characters at least and 128 bytes at most. One address makes 5 accounts each hour at most.
  NewAccount {
    /// The name of the account.
    name: String,
    /// The password of the account.
    password: Password,
  },
  /// Signs in to an account. The login service puts the name in lower case before it compares it.
  Login {
    /// The name of the account.
    name: String,
    /// The password of the account.
    password: Password,
  },
  /// Comes back to the place of the account in the queue, with the token of
  /// [`FromLogin::Queued`] and no new check of the password.
  Resume {
    /// The token of the place in the queue.
    token: Token,
  },
  /// Says that the client still waits in the queue, and asks for its place. A queued client sends
  /// it each 5 s, and the login service removes from the queue a client that sends none for 120 s.
  Wait,
  /// Makes a character of the admitted account at the start of the world, and gets the list of
  /// [`FromLogin::Characters`] again. The name holds 3 to 16 letters of ASCII, and the login
  /// service writes it with its first letter in upper case and the rest in lower case. An account
  /// holds 8 characters at most.
  NewCharacter {
    /// The name of the character.
    name: String,
  },
  /// Selects the character to play, from the list of [`FromLogin::Characters`].
  Select {
    /// The character to play.
    character: CharacterId,
  },
}

/// A message of the login service to a client, on a stream.
#[derive(Clone, Debug, PartialEq, Eq, Encode, Decode)]
pub enum FromLogin {
  /// The login service refuses the last message of the client.
  Refused(Refusal),
  /// The account waits in the queue of the world. Each login enters the queue, also when the world
  /// has a free slot, so no login passes an account that waits.
  Queued {
    /// The place of the account: the first account of the queue is at 0.
    position: u32,
    /// The count of accounts in the queue.
    length: u32,
    /// The estimate of the wait in seconds, from the admissions of the last 10 minutes.
    wait: u32,
    /// The token that brings the account back to its place with [`ToLogin::Resume`].
    resume: Token,
  },
  /// The account has a slot in the world for 60 s, and holds these characters.
  Characters(Vec<Character>),
  /// The session of the selected character, which the client gives to a gateway in [`Attach`].
  Session {
    /// The token of the session.
    token: Token,
  },
}

/// Why the login service refuses a message of a client.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Encode, Decode)]
pub enum Refusal {
  /// No account has this name and this password.
  Password,
  /// The client tries too often: the name had 5 failed logins from the address of the client in
  /// the last minute, the address had 30 failed logins in the last 10 minutes, or the address made
  /// 5 accounts in the last hour.
  Throttled,
  /// An account or a character holds the name already.
  Taken,
  /// The name breaks the rule of [`ToLogin::NewAccount`] or of [`ToLogin::NewCharacter`].
  Name,
  /// The account is online, and a second session would make a second copy of its character.
  Online,
  /// The account holds no character with this id.
  Character,
  /// The place in the queue or the slot in the world ended, so the client logs in again.
  Expired,
  /// The password of [`ToLogin::NewAccount`] holds fewer than 8 characters, or more than 128
  /// bytes.
  Weak,
  /// The account holds 8 characters already, so [`ToLogin::NewCharacter`] makes no more.
  Limit,
}

/// A character of an account, as the login service lists it.
#[derive(Clone, Debug, PartialEq, Eq, Encode, Decode)]
pub struct Character {
  /// The id of the character.
  pub id: CharacterId,
  /// The name of the character.
  pub name: String,
}

/// The first message of a client to a gateway after its [`Hello`](crate::Hello), on stream 0.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Encode, Decode)]
pub struct Attach {
  /// The token of [`FromLogin::Session`].
  pub session: Token,
}

/// A message of a client on stream 0, after its [`Attach`], which the gateway relays to the layer
/// writer of the player.
#[derive(Clone, Debug, PartialEq, Eq, Encode, Decode)]
pub enum FromClient {
  /// Writes a line of chat in a channel.
  Say {
    /// The name of the channel.
    channel: String,
    /// The line.
    text: String,
  },
  /// Ends the play of the character. The layer writer saves it before it removes its presence.
  Logout,
}

/// A message to a client on stream 0, from its gateway or from the layer writer of its player.
#[derive(Clone, Debug, PartialEq, Eq, Encode, Decode)]
pub enum ToClient {
  /// The gateway finds no session for the token of [`Attach`], so the client logs in again.
  NoSession,
  /// The player is in this zone, which the client names in each next [`Intents`]. The gateway
  /// sends it first after an [`Attach`], with the zone and the layer of the session, and a layer
  /// writer sends it when the player moves to another zone or to another layer of its zone.
  Moved {
    /// The zone of the player.
    zone: ZoneId,
    /// The number of the layer of the player in its zone.
    layer: u32,
  },
  /// The next zone holds as many players as it can, so the player stays at its border.
  Full {
    /// The zone that refused the player.
    zone: ZoneId,
  },
  /// A line of chat in a channel that the player reads.
  Line {
    /// The name of the channel.
    channel: String,
    /// The name of the character that wrote the line.
    from: String,
    /// The line.
    text: String,
  },
}

/// The datagram of a client, 20 times each second.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Encode, Decode)]
pub struct Intents {
  /// The zone of the player. The gateway relays the datagram to the writer of the layer of the
  /// player in that zone.
  pub zone: ZoneId,
  /// The number of the layer of the player in its zone.
  pub layer: u32,
  /// The tick of the last snapshot that the client received, or `None` before the first.
  pub ack: Option<Tick>,
  /// The last 3 inputs of the player, so the layer writer recovers the inputs of a lost datagram.
  pub inputs: [Input; 3],
}

/// The intent of a player for one tick. It never carries a position or a speed: the layer writer
/// reads the speed from its own table.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Encode, Decode)]
pub struct Input {
  /// The tick of the input.
  pub tick: Tick,
  /// The direction in which the player moves, or `None` when it stands.
  pub heading: Option<Angle>,
  /// The direction in which the player looks.
  pub view: Angle,
}

/// The datagram of a layer writer to a client, each tick: the entities that the player sees.
///
/// With no base, the snapshot is full: `entered` holds each visible entity, and `changed` and
/// `left` are empty. With a base, it is a delta against the snapshot of that tick, which the
/// client acknowledged. Each entity that the client held at the base, and that the delta does
/// not name, stands where [`Entity::ahead`] takes it, so an entity that walks straight costs no
/// byte. A [`Change`] gives the movement from that place.
#[derive(Clone, Debug, PartialEq, Eq, Encode, Decode)]
pub struct Snapshot {
  /// The tick of the snapshot.
  pub tick: Tick,
  /// The tick of the snapshot that the delta starts from, or `None` for a full snapshot.
  pub base: Option<Tick>,
  /// The entities that came into view since the base.
  pub entered: Vec<Entity>,
  /// The entities in view at the base and now, whose state changed.
  pub changed: Vec<Change>,
  /// The entities that left the view since the base.
  pub left: Vec<CharacterId>,
}

/// The state of an entity. Each entity is a character in this version of the protocol.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Encode, Decode)]
pub struct Entity {
  /// The id of the entity.
  pub id: CharacterId,
  /// The position of the entity.
  pub position: Position,
  /// The velocity of the entity.
  pub velocity: Velocity,
  /// The direction in which the entity looks.
  pub view: Angle,
  /// The animation that the entity plays, by its number in the content of the game.
  pub animation: u8,
  /// The hit points of the entity.
  pub hit_points: i32,
}

impl Entity {
  /// The entity `ticks` ticks later, as a delta takes each entity that it does not name: its
  /// position moves by its velocity in whole centimetres each tick, and each other value stays.
  #[must_use]
  pub fn ahead(self, ticks: u32) -> Self {
    let ticks = i32::try_from(ticks).unwrap_or(i32::MAX);
    let each = TICKS_PER_SECOND.cast_signed();
    let by = |speed: i16| (i32::from(speed) / each).saturating_mul(ticks);
    let Position { x, y, z } = self.position;
    let Velocity { x: vx, y: vy, z: vz } = self.velocity;
    let position = Position {
      x: x.saturating_add(by(vx)),
      y: y.saturating_add(by(vy)),
      z: z.saturating_add(by(vz)),
    };
    Self { position, ..self }
  }
}

/// The new state of an entity in a delta, with its movement since the base in place of its
/// position. Each other field is `None` when its value is the value at the base, so a crowd that
/// walks sends its movements and little more.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Encode, Decode)]
pub struct Change {
  /// The id of the entity.
  pub id: CharacterId,
  /// The movement of the entity from the place where [`Entity::ahead`] takes it at the base.
  pub movement: Offset,
  /// The velocity of the entity, or `None` when it did not change since the base.
  pub velocity: Option<Velocity>,
  /// The direction in which the entity looks, or `None` when it did not change since the base.
  pub view: Option<Angle>,
  /// The animation that the entity plays, by its number in the content of the game, or `None`
  /// when it did not change since the base.
  pub animation: Option<u8>,
  /// The hit points of the entity, or `None` when they did not change since the base.
  pub hit_points: Option<i32>,
}

/// A message of a gateway to a layer writer, on a stream.
#[derive(Clone, Debug, PartialEq, Eq, Encode, Decode)]
pub enum ToZone {
  /// A client attached on a link with the session of a character whose zone is this zone.
  Join {
    /// The link of the client.
    link: Link,
    /// The account of the session.
    account: AccountId,
    /// The character of the session.
    character: CharacterId,
  },
  /// A message of the client of a link.
  Client(Relayed<FromClient>),
  /// The connection of a link closed. The layer writer keeps the player in the world for 30 s.
  Leave {
    /// The link that closed.
    link: Link,
  },
}

/// A message that a gateway relays between one link and a layer writer on a stream: a
/// [`FromClient`] in [`ToZone::Client`], and a [`ToClient`] from the writer. A datagram of a link
/// goes as [`crate::relayed`] makes it. The writer drops a message of a link that is not the link
/// of one of its players.
#[derive(Clone, Debug, PartialEq, Eq, Encode, Decode)]
pub struct Relayed<M> {
  /// The link of the client.
  pub link: Link,
  /// The message.
  pub message: M,
}

/// A message of a layer writer to the writer of a layer of a next zone, on a stream.
#[derive(Clone, Debug, PartialEq, Eq, Encode, Decode)]
pub enum ToNeighbour {
  /// Gives a player that crosses the border. The writer that sends it takes no more inputs of the
  /// player.
  Transfer {
    /// The account of the player, whose presence the target writes.
    account: AccountId,
    /// The link of the client, the only link whose inputs the target takes for the player.
    link: Link,
    /// The state of the character of the player.
    state: Entity,
    /// The version of the character, which the target adds 1 to when it saves the character.
    version: i64,
  },
  /// The target holds the character and saved it, so the source drops it and sends
  /// [`ToClient::Moved`].
  Accepted {
    /// The character that the target holds.
    character: CharacterId,
  },
  /// The target holds as many players as it can, so the source keeps the player at the border.
  Full {
    /// The character that the target refused.
    character: CharacterId,
  },
  /// The entities within 48 m of the border, each tick. The target shows them, and owns none.
  Border {
    /// The tick of the source.
    tick: Tick,
    /// The entities near the border.
    entities: Vec<Entity>,
  },
}

/// The state of a character that a save keeps, as the layer writer holds it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Encode, Decode)]
pub struct State {
  /// The point under the character, in whole centimetres, as the step of `mmo-collide` gives it.
  pub position: Position,
  /// The direction of view.
  pub yaw: Angle,
  /// The hit points.
  pub hit_points: i32,
}

/// The state of one character in a [`Batch`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Encode, Decode)]
pub struct Save {
  /// The character.
  pub character: CharacterId,
  /// Its state.
  pub state: State,
  /// Its version, which must be greater than the saved one.
  pub version: i64,
}

/// The saves that a layer writer sends to the keeper, one for each character at most.
///
/// A batch goes in one frame, so it holds [`SAVES`] saves at most: a writer with more dirty
/// characters sends more batches.
#[derive(Clone, Debug, PartialEq, Eq, Encode, Decode)]
pub struct Batch {
  /// The shard of the writer.
  pub shard: ShardId,
  /// The layer of the writer.
  pub layer: Layer,
  /// The epoch that the writer claimed with [`ToKeeper::Claim`].
  pub epoch: Epoch,
  /// The saves.
  pub saves: Vec<Save>,
}

/// The most saves of a [`Batch`] that fit in one frame, whatever their values.
pub const SAVES: usize = 1926;

/// A message of a layer writer to the keeper.
///
/// The writer opens one bidirectional stream to the keeper. After the hello of each side, the
/// keeper answers each message with one [`FromKeeper`], in the order of the messages.
#[derive(Clone, Debug, PartialEq, Eq, Encode, Decode)]
pub enum ToKeeper {
  /// Asks for the next epoch of a layer. The writer claims it before it renews the lease of the
  /// layer in the directory with it.
  Claim {
    /// The shard of the layer.
    shard: ShardId,
    /// The layer that the writer takes.
    layer: Layer,
  },
  /// Asks the keeper to write a batch in one transaction.
  Save(Batch),
}

/// The answer of the keeper to a [`ToKeeper`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Encode, Decode)]
pub enum FromKeeper {
  /// The keeper wrote the next epoch of the layer, so it refuses each batch of the writers before.
  Claimed {
    /// The epoch of the writer that claimed the layer.
    epoch: Epoch,
  },
  /// The keeper wrote the batch. It wrote only the saves whose version is greater than the saved
  /// one, so no save of the batch stays to send.
  Saved {
    /// The count of the characters that the keeper wrote.
    written: u64,
  },
  /// The keeper wrote nothing.
  Refused(KeeperRefusal),
}

/// Why the keeper writes nothing for a [`ToKeeper`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Encode, Decode)]
pub enum KeeperRefusal {
  /// The table `zones` holds no zone of this id.
  NoZone,
  /// A newer writer claimed the layer, so the writer of the batch lost it. A layer that no writer
  /// claimed holds the epoch 0.
  Stale {
    /// The epoch of the last writer that claimed the layer.
    held: Epoch,
  },
  /// The keeper cannot write in PostgreSQL now. The writer keeps its saves and sends them again.
  Unavailable,
}

#[cfg(test)]
mod tests {
  use std::fmt::Debug;

  use bitcode::{DecodeOwned, Encode};

  use super::{
    Attach, Batch, Change, Character, Entity, FromClient, FromKeeper, FromLogin, Input, Intents,
    KeeperRefusal, Refusal, Relayed, SAVES, Save, Snapshot, State, ToClient, ToKeeper, ToLogin,
    ToNeighbour, ToZone,
  };
  use crate::{
    AccountId, Angle, CharacterId, Epoch, FRAME, Layer, Link, Offset, Password, Position, ShardId,
    Tick, Token, Velocity, ZoneId, datagram, decode, encode,
  };

  const TOKEN: Token = Token([0xa5; 32]);
  const LINK: Link = Link(0x0123_4567_89ab_cdef);

  const STILL: State =
    State { position: Position { x: 0, y: 0, z: 0 }, yaw: Angle(16384), hit_points: 100 };

  const MALFORMED: &str = "the bytes are not a message of this version of the protocol";

  fn round_trip<M: Encode + DecodeOwned + PartialEq + Debug>(messages: &[M]) {
    for message in messages {
      let bytes = encode(message);
      assert_eq!(decode::<M>(&bytes).as_ref().map_err(ToString::to_string), Ok(message));
      let longer = [bytes.as_slice(), &[0]].concat();
      for malformed in (0..bytes.len()).map(|end| &bytes[..end]).chain([longer.as_slice()]) {
        let refused = decode::<M>(malformed).map_err(|error| error.to_string());
        assert_eq!(refused, Err(MALFORMED.to_owned()), "{malformed:?}");
      }
    }
  }

  fn entity(id: i64) -> Entity {
    Entity {
      id: CharacterId(id),
      position: Position { x: -102_400, y: 51_200, z: 25 },
      velocity: Velocity { x: 600, y: -600, z: 0 },
      view: Angle(49152),
      animation: 3,
      hit_points: 250,
    }
  }

  fn intents() -> Intents {
    let input = |tick, heading| Input { tick: Tick(tick), heading, view: Angle(16384) };
    Intents {
      zone: ZoneId(7),
      layer: 3,
      ack: Some(Tick(997)),
      inputs: [input(998, None), input(999, Some(Angle(0))), input(1000, Some(Angle(32768)))],
    }
  }

  #[test]
  fn each_message_of_the_login_link_comes_back_equal() {
    round_trip(&[
      ToLogin::NewAccount { name: "ada".to_owned(), password: Password("pass word".to_owned()) },
      ToLogin::Login { name: "ada".to_owned(), password: Password("pass word".to_owned()) },
      ToLogin::Resume { token: TOKEN },
      ToLogin::Wait,
      ToLogin::NewCharacter { name: "Brom".to_owned() },
      ToLogin::Select { character: CharacterId(42) },
    ]);
    let refusals = [
      Refusal::Password,
      Refusal::Throttled,
      Refusal::Taken,
      Refusal::Name,
      Refusal::Online,
      Refusal::Character,
      Refusal::Expired,
      Refusal::Weak,
      Refusal::Limit,
    ];
    round_trip(&refusals.map(FromLogin::Refused));
    round_trip(&[
      FromLogin::Queued { position: 0, length: 1000, wait: 600, resume: TOKEN },
      FromLogin::Characters(vec![]),
      FromLogin::Characters(vec![
        Character { id: CharacterId(1), name: "Ada".to_owned() },
        Character { id: CharacterId(i64::MAX), name: "Grace".to_owned() },
      ]),
      FromLogin::Session { token: TOKEN },
    ]);
  }

  #[test]
  fn each_message_of_stream_0_comes_back_equal() {
    round_trip(&[Attach { session: TOKEN }]);
    round_trip(&[
      FromClient::Say { channel: "world".to_owned(), text: "hello, all".to_owned() },
      FromClient::Logout,
    ]);
    round_trip(&[
      ToClient::NoSession,
      ToClient::Moved { zone: ZoneId(2), layer: u32::MAX },
      ToClient::Full { zone: ZoneId(-1) },
      ToClient::Line { channel: "world".to_owned(), from: "Ada".to_owned(), text: "hi".to_owned() },
    ]);
  }

  #[test]
  fn each_datagram_comes_back_equal() {
    round_trip(&[intents(), Intents { ack: None, ..intents() }]);
    let change = Change {
      id: CharacterId(9),
      movement: Offset { x: 30, y: -30, z: 0 },
      velocity: Some(Velocity { x: 600, y: -600, z: 0 }),
      view: Some(Angle(100)),
      animation: Some(1),
      hit_points: Some(0),
    };
    let moved = Change { velocity: None, view: None, animation: None, hit_points: None, ..change };
    round_trip(&[
      Snapshot {
        tick: Tick(1000),
        base: None,
        entered: vec![entity(1), entity(2)],
        changed: vec![],
        left: vec![],
      },
      Snapshot {
        tick: Tick(1000),
        base: Some(Tick(997)),
        entered: vec![entity(3)],
        changed: vec![change, moved],
        left: vec![CharacterId(4), CharacterId(5)],
      },
    ]);
  }

  #[test]
  fn an_entity_ahead_moves_by_its_velocity_in_whole_centimetres_each_tick() {
    let walker = Entity { velocity: Velocity { x: 600, y: -30, z: -19 }, ..entity(1) };
    let Position { x, y, z } = walker.position;
    let ahead = Entity { position: Position { x: x + 90, y: y - 3, z }, ..walker };
    assert_eq!(walker.ahead(3), ahead);
    assert_eq!(walker.ahead(1).ahead(2), ahead);
    assert_eq!(walker.ahead(0), walker);
  }

  #[test]
  fn each_message_between_the_servers_comes_back_equal() {
    round_trip(&[
      ToZone::Join { link: LINK, account: AccountId(11), character: CharacterId(42) },
      ToZone::Client(Relayed { link: LINK, message: FromClient::Logout }),
      ToZone::Leave { link: LINK },
    ]);
    round_trip(&[Relayed { link: LINK, message: ToClient::Moved { zone: ZoneId(3), layer: 2 } }]);
    round_trip(&[ToNeighbour::Transfer {
      account: AccountId(11),
      link: LINK,
      state: entity(42),
      version: 17,
    }]);
    round_trip(&[
      ToNeighbour::Accepted { character: CharacterId(42) },
      ToNeighbour::Full { character: CharacterId(42) },
      ToNeighbour::Border { tick: Tick(1000), entities: vec![entity(1), entity(2)] },
    ]);
  }

  #[test]
  fn the_bytes_of_a_datagram_stay_the_bytes_of_this_version() {
    let bytes = [
      4, 7, 4, 3, 1, 2, 229, 3, 2, 230, 3, 231, 3, 232, 3, 6, 0, 0, 0, 0, 128, 0, 0, 64, 0, 64, 0,
      64,
    ];
    assert_eq!(datagram(&intents()).as_ref().map_err(ToString::to_string), Ok(&bytes.to_vec()));
  }

  #[test]
  fn no_bytes_and_the_bytes_of_another_message_are_refused() {
    for bytes in [&[][..], &encode(&intents())] {
      assert_eq!(
        decode::<ToLogin>(bytes).map_err(|error| error.to_string()),
        Err(MALFORMED.to_owned())
      );
    }
  }
  #[test]
  fn each_message_between_a_writer_and_the_keeper_comes_back_equal() {
    let state = State { position: Position { x: -102_400, y: 51_200, z: 25 }, ..STILL };
    let saves = vec![
      Save { character: CharacterId(42), state, version: 17 },
      Save { character: CharacterId(i64::MAX), state: STILL, version: 0 },
    ];
    round_trip(&[
      ToKeeper::Claim { shard: ShardId(2), layer: Layer { zone: ZoneId(7), number: 3 } },
      ToKeeper::Save(Batch {
        shard: ShardId(2),
        layer: Layer { zone: ZoneId(7), number: 3 },
        epoch: Epoch(3),
        saves,
      }),
      ToKeeper::Save(Batch {
        shard: ShardId(u32::MAX),
        layer: Layer { zone: ZoneId(-1), number: u32::MAX },
        epoch: Epoch(0),
        saves: vec![],
      }),
    ]);
    round_trip(&[
      FromKeeper::Claimed { epoch: Epoch(4) },
      FromKeeper::Saved { written: 2 },
      FromKeeper::Refused(KeeperRefusal::NoZone),
      FromKeeper::Refused(KeeperRefusal::Stale { held: Epoch(5) }),
      FromKeeper::Refused(KeeperRefusal::Unavailable),
    ]);
  }

  #[test]
  fn a_batch_of_the_most_saves_fits_in_a_frame_whatever_their_values() {
    let far = |at: i32, yaw: u16, id: i64| Save {
      character: CharacterId(id),
      state: State { position: Position { x: at, y: at, z: at }, yaw: Angle(yaw), hit_points: at },
      version: id,
    };
    let ends = [far(i32::MIN, 0, i64::MIN), far(i32::MAX, u16::MAX, i64::MAX)];
    let bytes = |count: usize| {
      let saves = ends.into_iter().cycle().take(count).collect();
      let layer = Layer { zone: ZoneId(i32::MIN), number: u32::MAX };
      let batch = Batch { shard: ShardId(u32::MAX), layer, epoch: Epoch(i64::MAX), saves };
      encode(&ToKeeper::Save(batch)).len()
    };
    assert_eq!([bytes(SAVES), bytes(SAVES + 1)], [FRAME - 16, FRAME + 18]);
  }
}
