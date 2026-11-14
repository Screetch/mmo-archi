# Changelog

## [0.3.0](https://github.com/Screetch/mmo-archi/compare/v0.2.0...v0.3.0) (2026-11-14)


### ⚠ BREAKING CHANGES

* **bots:** Run stage 7 of the load test on 3 realms and a full realm ([#83](https://github.com/Screetch/mmo-archi/issues/83))
* **login:** List the realms with their attributes and population ([#82](https://github.com/Screetch/mmo-archi/issues/82))
* **protocol:** Rename the shard to the realm everywhere ([#79](https://github.com/Screetch/mmo-archi/issues/79))
* **bots:** Run stage 5 of the load test, the drain of a layer ([#73](https://github.com/Screetch/mmo-archi/issues/73))
* **zone:** Show the entities near a border to the zone across it ([#68](https://github.com/Screetch/mmo-archi/issues/68))
* **zone:** Read the zone across each border from the map ([#67](https://github.com/Screetch/mmo-archi/issues/67))
* **zone:** Hand a player over to the next zone at its border ([#64](https://github.com/Screetch/mmo-archi/issues/64))
* **store:** Remove the parties that the game does not have ([#61](https://github.com/Screetch/mmo-archi/issues/61))
* **login:** Keep each character in the shard that it belongs to ([#58](https://github.com/Screetch/mmo-archi/issues/58))
* **zone:** Close a layer that holds no player for 30 s ([#55](https://github.com/Screetch/mmo-archi/issues/55))
* **zone:** Run each layer of a zone with its own writer from a pool ([#53](https://github.com/Screetch/mmo-archi/issues/53))
* **keeper:** Give each layer of a shard its own epoch ([#50](https://github.com/Screetch/mmo-archi/issues/50))

### Features

* **bots:** Check the sight between shards and the layer of crossings ([#70](https://github.com/Screetch/mmo-archi/issues/70)) ([8937559](https://github.com/Screetch/mmo-archi/commit/8937559f8d2dde1fed814c689fa07462b1e100a0))
* **bots:** Prove the layers and the shards with at most 500 bots ([#57](https://github.com/Screetch/mmo-archi/issues/57)) ([ef0583e](https://github.com/Screetch/mmo-archi/commit/ef0583e6b0cff1befe3dd821bdca0956a469b3b8))
* **bots:** Prove the queue and the recovery with at most 500 bots ([#62](https://github.com/Screetch/mmo-archi/issues/62)) ([1c1af03](https://github.com/Screetch/mmo-archi/commit/1c1af03f189540c4d6a252595f6f1160b02fafca))
* **bots:** Run stage 5 of the load test, the drain of a layer ([#73](https://github.com/Screetch/mmo-archi/issues/73)) ([419f9fa](https://github.com/Screetch/mmo-archi/commit/419f9fa01a329fd06c97720d4ede15ecbbd8ec0d))
* **bots:** Run stage 7 of the load test on 3 realms and a full realm ([#83](https://github.com/Screetch/mmo-archi/issues/83)) ([cbc5192](https://github.com/Screetch/mmo-archi/commit/cbc51922045c5791a5fee49239c7bdf5a75fe039))
* **keeper:** Give each layer of a shard its own epoch ([#50](https://github.com/Screetch/mmo-archi/issues/50)) ([b249636](https://github.com/Screetch/mmo-archi/commit/b249636e6f4c8bde9184a0e6efb41965273bfedf))
* **login:** List the realms with their attributes and population ([#82](https://github.com/Screetch/mmo-archi/issues/82)) ([29ccb1d](https://github.com/Screetch/mmo-archi/commit/29ccb1d7c0bb0a2cf7ec3acd109de860622f77fe))
* **store:** Keep the layers of each zone of a shard in a directory ([#49](https://github.com/Screetch/mmo-archi/issues/49)) ([1c62f61](https://github.com/Screetch/mmo-archi/commit/1c62f6154fb0d4bd2131f4ce24eb48bae5230c83))
* **store:** Put each zone on a square of the map of the world ([#63](https://github.com/Screetch/mmo-archi/issues/63)) ([6540652](https://github.com/Screetch/mmo-archi/commit/654065299493272ade427c61984fd8bec27eb4d2))
* **zone:** Close a layer that holds no player for 30 s ([#55](https://github.com/Screetch/mmo-archi/issues/55)) ([8e7c7c9](https://github.com/Screetch/mmo-archi/commit/8e7c7c92e9821bc21286a09cd180dd4462a05b6f))
* **zone:** Drain a sparse layer and move its players to another layer ([#69](https://github.com/Screetch/mmo-archi/issues/69)) ([21d6a38](https://github.com/Screetch/mmo-archi/commit/21d6a38818b9768bbdc6483225080b83f4d719fa))
* **zone:** Hand a player over to the next zone at its border ([#64](https://github.com/Screetch/mmo-archi/issues/64)) ([6b3c8ac](https://github.com/Screetch/mmo-archi/commit/6b3c8ac5d78c692472ab037ab2101922168d3bbb))
* **zone:** Read the zone across each border from the map ([#67](https://github.com/Screetch/mmo-archi/issues/67)) ([bbe7d71](https://github.com/Screetch/mmo-archi/commit/bbe7d71711bf3f23c2af96994d92509f65a998a5))
* **zone:** Run each layer of a zone with its own writer from a pool ([#53](https://github.com/Screetch/mmo-archi/issues/53)) ([2132d9f](https://github.com/Screetch/mmo-archi/commit/2132d9f7db39ce46ebb6e2e9e6652a46d3bcf2b6))
* **zone:** Show the entities near a border to the zone across it ([#68](https://github.com/Screetch/mmo-archi/issues/68)) ([fa844e5](https://github.com/Screetch/mmo-archi/commit/fa844e50e37fc8232c266394f72d4b23390c79a2))


### Fixes

* **bots:** Refuse a count of bots over 500 ([#56](https://github.com/Screetch/mmo-archi/issues/56)) ([a3a1c21](https://github.com/Screetch/mmo-archi/commit/a3a1c213f7b05169b3cd0636a812891fb62b98e2))
* **bots:** Seed each bot in the shard that it plays in ([#59](https://github.com/Screetch/mmo-archi/issues/59)) ([dfa9f12](https://github.com/Screetch/mmo-archi/commit/dfa9f120fcc16d9aac212e9ebf74425bd8edf8cb))
* **bots:** Send no intents before the gateway names the layer ([#80](https://github.com/Screetch/mmo-archi/issues/80)) ([2bdadd0](https://github.com/Screetch/mmo-archi/commit/2bdadd04e4ea82c8d8e130ba00aa34e3c6b68b22))
* **conductor:** Merge a PR only on a head that holds the current main ([#66](https://github.com/Screetch/mmo-archi/issues/66)) ([dda1a21](https://github.com/Screetch/mmo-archi/commit/dda1a21f64d8cf6e8aa872445f20794d8446f43e))
* **conductor:** Merge a PR only when the gates and the load passed ([#77](https://github.com/Screetch/mmo-archi/issues/77)) ([9466978](https://github.com/Screetch/mmo-archi/commit/94669789ffe07830a64e193eb7db3d5f3ea03328))
* **gateway:** Attach a client to the layer that holds its player ([#71](https://github.com/Screetch/mmo-archi/issues/71)) ([baee621](https://github.com/Screetch/mmo-archi/commit/baee6219b874e4b7fb6a0f5bd2de5c8c7ea3b430))
* **login:** Keep each character in the shard that it belongs to ([#58](https://github.com/Screetch/mmo-archi/issues/58)) ([cc5772c](https://github.com/Screetch/mmo-archi/commit/cc5772cefff8c1eec1eb149c0d9e9e7f75187767))
* **login:** Keep the order of entry in the queue within a millisecond ([#81](https://github.com/Screetch/mmo-archi/issues/81)) ([da1ef0a](https://github.com/Screetch/mmo-archi/commit/da1ef0a8726f88eb474320c2264b4c380a26c80e))
* **store:** Remove the parties that the game does not have ([#61](https://github.com/Screetch/mmo-archi/issues/61)) ([0403a1d](https://github.com/Screetch/mmo-archi/commit/0403a1d26e61d899f8459ab5d56833c27bf535b8))
* **store:** Start Redis again on a new port when its port is taken ([#47](https://github.com/Screetch/mmo-archi/issues/47)) ([2b6cfa6](https://github.com/Screetch/mmo-archi/commit/2b6cfa6acd4f60b3005c68ad01a36acbdb80aa12))
* **zone:** Call the writer of a layer the layer writer in each crate ([#60](https://github.com/Screetch/mmo-archi/issues/60)) ([52cc9f6](https://github.com/Screetch/mmo-archi/commit/52cc9f63da5a1dfd98e3da1a1e4526d6bc85dfad))
* **zone:** Keep the entries of a layer writer in order in Redis ([#74](https://github.com/Screetch/mmo-archi/issues/74)) ([4cc0f2e](https://github.com/Screetch/mmo-archi/commit/4cc0f2e399d93b1fa98d1b48532ac1fff5e05b16))
* **zone:** Let each bot of the load test with crossers play ([#76](https://github.com/Screetch/mmo-archi/issues/76)) ([5061649](https://github.com/Screetch/mmo-archi/commit/50616497b51b8e7f83da78b8b506b96823c77758))


### Chores

* **protocol:** Rename the shard to the realm everywhere ([#79](https://github.com/Screetch/mmo-archi/issues/79)) ([830a18f](https://github.com/Screetch/mmo-archi/commit/830a18f85816e36021dd413e0dcb22ba265dde42))

## [0.2.0](https://github.com/Screetch/mmo-archi/compare/v0.1.0...v0.2.0) (2026-11-12)


### ⚠ BREAKING CHANGES

* **gateway:** Relay each snapshot as bytes straight to its client
* **zone:** Send only the entities that leave their straight walk ([#40](https://github.com/Screetch/mmo-archi/issues/40))

### Fixes

* **bots:** Take the zone that the gateway names before the play ([6d180ad](https://github.com/Screetch/mmo-archi/commit/6d180ad38dcf1155e9e1c4cef540947ca1ccaa28))
* **bots:** Test that a walk keeps its heading until its next turn ([#38](https://github.com/Screetch/mmo-archi/issues/38)) ([9fa30f2](https://github.com/Screetch/mmo-archi/commit/9fa30f272adca421f76d86b37660ddf54f3d3917))
* **ci:** Skip the crates with no library in the contract check ([#36](https://github.com/Screetch/mmo-archi/issues/36)) ([9298dcf](https://github.com/Screetch/mmo-archi/commit/9298dcf86373ac291120a200e1f69166350eb9aa))
* **gateway:** Build the TLS of the gateway and keeper with mmo-link ([#35](https://github.com/Screetch/mmo-archi/issues/35)) ([ba515ce](https://github.com/Screetch/mmo-archi/commit/ba515ce8d5bb0067fc11d43a75fde72f46f54bfd))
* **gateway:** Open the gateway of a test on a port that it holds ([#39](https://github.com/Screetch/mmo-archi/issues/39)) ([22c3d3d](https://github.com/Screetch/mmo-archi/commit/22c3d3d4f355b58a5cdeee43c38cdb893c95839b))


### Performance

* **gateway:** Relay each snapshot as bytes straight to its client ([6d180ad](https://github.com/Screetch/mmo-archi/commit/6d180ad38dcf1155e9e1c4cef540947ca1ccaa28))
* **zone:** Send only the entities that leave their straight walk ([#40](https://github.com/Screetch/mmo-archi/issues/40)) ([3f5edf5](https://github.com/Screetch/mmo-archi/commit/3f5edf59ef3097596ac3b553851598b7ce342826))

## 0.1.0 (2026-11-11)


### ⚠ BREAKING CHANGES

* **client:** Open a window and play the game with mmo-play ([#34](https://github.com/Screetch/mmo-archi/issues/34))
* **gateway:** Tell the client the zone of its session first ([#33](https://github.com/Screetch/mmo-archi/issues/33))
* **bots:** Test the load of the game with headless clients ([#32](https://github.com/Screetch/mmo-archi/issues/32))
* **store:** Refuse a ninth character of an account in the database ([#24](https://github.com/Screetch/mmo-archi/issues/24))
* **login:** Refuse a weak password and a ninth character ([#21](https://github.com/Screetch/mmo-archi/issues/21))
* **login:** Count the failed logins by name and by address ([#17](https://github.com/Screetch/mmo-archi/issues/17))
* **login:** Let a client make its account and its characters ([#14](https://github.com/Screetch/mmo-archi/issues/14))

### Features

* **bots:** Test the load of the game with headless clients ([#32](https://github.com/Screetch/mmo-archi/issues/32)) ([e926705](https://github.com/Screetch/mmo-archi/commit/e926705975ad66164bde2ee1da9e158959bc7cec))
* **client:** Log in through the queue and link to a gateway ([#26](https://github.com/Screetch/mmo-archi/issues/26)) ([347fdbf](https://github.com/Screetch/mmo-archi/commit/347fdbf3347e2fc40e7098930ae95f80d08bfda7))
* **client:** Open a window and play the game with mmo-play ([#34](https://github.com/Screetch/mmo-archi/issues/34)) ([e2fa515](https://github.com/Screetch/mmo-archi/commit/e2fa5157a6ad28a7adb45e9c025b1dbf45e5f7d6))
* **client:** Predict the player and show the world of the snapshots ([#23](https://github.com/Screetch/mmo-archi/issues/23)) ([7a1c282](https://github.com/Screetch/mmo-archi/commit/7a1c282c637e482b23cedfaf99eb55419a62eb08))
* **collide:** Add the shared step of a body in a static world ([#4](https://github.com/Screetch/mmo-archi/issues/4)) ([07341a0](https://github.com/Screetch/mmo-archi/commit/07341a0d8f054f95a5d9e37331a324aa591cccfb))
* **collide:** Share the ground of a zone with each client ([#22](https://github.com/Screetch/mmo-archi/issues/22)) ([95751ce](https://github.com/Screetch/mmo-archi/commit/95751ceb90960b72e8f52d882b47fc0773d4d41d))
* **gateway:** Relay each client to the zone writer of its player ([#20](https://github.com/Screetch/mmo-archi/issues/20)) ([fca21f8](https://github.com/Screetch/mmo-archi/commit/fca21f8ee9595133ab6d5506257c0ca2508d18b7))
* **gateway:** Tell the client the zone of its session first ([#33](https://github.com/Screetch/mmo-archi/issues/33)) ([553b8ad](https://github.com/Screetch/mmo-archi/commit/553b8adf11c99869357411d569144db205f331f8))
* **keeper:** Write the game data in PostgreSQL from one keeper ([#10](https://github.com/Screetch/mmo-archi/issues/10)) ([a042018](https://github.com/Screetch/mmo-archi/commit/a042018260c7a61d5680b7239a9c3942de580b7b))
* **local:** Start, check and stop the whole stack with one script ([#28](https://github.com/Screetch/mmo-archi/issues/28)) ([3377265](https://github.com/Screetch/mmo-archi/commit/337726575a9e4f91698ac675048326bbff70663c))
* **login:** Add the login service with the admission queue in Redis ([#9](https://github.com/Screetch/mmo-archi/issues/9)) ([f4af9fd](https://github.com/Screetch/mmo-archi/commit/f4af9fd5605b65dd381315c16cf034c9c0d8a886))
* **login:** Let a client make its account and its characters ([#14](https://github.com/Screetch/mmo-archi/issues/14)) ([49b310c](https://github.com/Screetch/mmo-archi/commit/49b310cfd77e4c4efc9c5300cb268ee89af53085))
* **login:** Serve the login service over QUIC ([#11](https://github.com/Screetch/mmo-archi/issues/11)) ([4847170](https://github.com/Screetch/mmo-archi/commit/4847170ef448e3cdf921b85d63baf71b36657e23))
* **protocol:** Add the messages, the frames and the codec of the game ([#2](https://github.com/Screetch/mmo-archi/issues/2)) ([b22521e](https://github.com/Screetch/mmo-archi/commit/b22521ed38f70cb38beed597cbeb8997060eec7c))
* **render:** Draw the ground and the characters with wgpu ([#31](https://github.com/Screetch/mmo-archi/issues/31)) ([ad36108](https://github.com/Screetch/mmo-archi/commit/ad36108c593619e2395dd2a7b9ef01e6a2c65e90))
* **render:** Load skinned glTF models and pose them in the Idle clip ([#30](https://github.com/Screetch/mmo-archi/issues/30)) ([a937188](https://github.com/Screetch/mmo-archi/commit/a9371885bb66f4d93b7c394ed6104a889108e102))
* **store:** Keep accounts, characters and zone saves in PostgreSQL ([#6](https://github.com/Screetch/mmo-archi/issues/6)) ([146f230](https://github.com/Screetch/mmo-archi/commit/146f23081e84637b4b312114bc0ee9bb213298e4))
* **zone:** Claim the zone epoch and save the state of its players ([#19](https://github.com/Screetch/mmo-archi/issues/19)) ([f5d867b](https://github.com/Screetch/mmo-archi/commit/f5d867bc151f9be41fb9722f9619f04a9a3b734c))
* **zone:** Report the time of each tick through Redis ([#29](https://github.com/Screetch/mmo-archi/issues/29)) ([59ce192](https://github.com/Screetch/mmo-archi/commit/59ce192745aeb3b1e9f9a42ebb9be72a5523b990))
* **zone:** Run one zone for the players of the gateways ([#12](https://github.com/Screetch/mmo-archi/issues/12)) ([6cfadbd](https://github.com/Screetch/mmo-archi/commit/6cfadbd227ee87bf8c6ff56767bfa50175b244b4))


### Fixes

* **ci:** Pack and sum the binaries on Windows ([#13](https://github.com/Screetch/mmo-archi/issues/13)) ([6df3d30](https://github.com/Screetch/mmo-archi/commit/6df3d3007a7608dd6328954c3b57f372d841f7f2))
* **ci:** Pass the contract when no tag v* stands ([#5](https://github.com/Screetch/mmo-archi/issues/5)) ([eae01eb](https://github.com/Screetch/mmo-archi/commit/eae01eb668b4d260aecb2944cb5d06f02414b5cd))
* **ci:** Strip the carriage return from binary names on Windows ([#15](https://github.com/Screetch/mmo-archi/issues/15)) ([0c01890](https://github.com/Screetch/mmo-archi/commit/0c0189032db9f8053b1a6c6b920237c4d8992455))
* **login:** Count the failed logins by name and by address ([#17](https://github.com/Screetch/mmo-archi/issues/17)) ([3d0a3f7](https://github.com/Screetch/mmo-archi/commit/3d0a3f7d8c4f71515ef3060ce831947973877cfe))
* **login:** Refuse a weak password and a ninth character ([#21](https://github.com/Screetch/mmo-archi/issues/21)) ([f5b304f](https://github.com/Screetch/mmo-archi/commit/f5b304ff7854da4700b71732b1465e63738e1c37))
* **protocol:** Build the version of each test hello from PROTOCOL ([#18](https://github.com/Screetch/mmo-archi/issues/18)) ([4704c27](https://github.com/Screetch/mmo-archi/commit/4704c2768cf1180b8794146da7e5038c539166b2))
* **store:** Refuse a ninth character of an account in the database ([#24](https://github.com/Screetch/mmo-archi/issues/24)) ([1904a9a](https://github.com/Screetch/mmo-archi/commit/1904a9ae7f3f2cdd6982ec87ef72d84024a68415))
* **zone:** Claim the epoch and send each save through the keeper ([#25](https://github.com/Screetch/mmo-archi/issues/25)) ([737b286](https://github.com/Screetch/mmo-archi/commit/737b286d25c067710867b7d4d2c78f1b703ca5f4))

## Changelog

Each release of mmo-archi is listed here, the newest first. release-please writes the entry of each release from the
titles of the pull requests that it holds, as [the template of a pull request](.github/pull_request_template.md) says.
