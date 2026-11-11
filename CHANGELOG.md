# Changelog

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
