# Changelog

## [1.0.0-beta.3](https://github.com/gorules/zen/compare/zen-engine-v1.0.0-beta.2...zen-engine-v1.0.0-beta.3) (2026-07-07)


### Features

* natural language ([#477](https://github.com/gorules/zen/issues/477)) ([6858c1b](https://github.com/gorules/zen/commit/6858c1bd045e9e00ade70b734f4a7ca065a36659))


### Dependencies

* The following workspace dependencies were updated
  * dependencies
    * zen-types bumped from 1.0.0-beta.2 to 1.0.0-beta.3
    * zen-expression bumped from 1.0.0-beta.2 to 1.0.0-beta.3
    * zen-tmpl bumped from 1.0.0-beta.2 to 1.0.0-beta.3

## [1.0.0-beta.2](https://github.com/gorules/zen/compare/zen-engine-v1.0.0-beta.1...zen-engine-v1.0.0-beta.2) (2026-06-26)


### Bug Fixes

* trigger release v1.0.0-beta.2 ([#470](https://github.com/gorules/zen/issues/470)) ([355b86a](https://github.com/gorules/zen/commit/355b86a57c2cb63f3aba978897f6c77c35b53e80))

## [1.0.0-beta.1](https://github.com/gorules/zen/compare/zen-engine-v1.0.0-beta.0...zen-engine-v1.0.0-beta.1) (2026-06-25)


### Bug Fixes

* switch releases to tag based workflow triggers ([#465](https://github.com/gorules/zen/issues/465)) ([bb53a52](https://github.com/gorules/zen/commit/bb53a5298fc6e419052c6ab1f36b8839aa20f3fb))


### Dependencies

* The following workspace dependencies were updated
  * dependencies
    * zen-types bumped from 1.0.0-beta.0 to 1.0.0-beta.1
    * zen-expression bumped from 1.0.0-beta.0 to 1.0.0-beta.1
    * zen-tmpl bumped from 1.0.0-beta.0 to 1.0.0-beta.1

## [1.0.0-beta.0](https://github.com/gorules/zen/compare/zen-engine-v0.55.1...zen-engine-v1.0.0-beta.0) (2026-06-25)


### Features

* add bigjs to function node; ([#77](https://github.com/gorules/zen/issues/77)) ([a03978f](https://github.com/gorules/zen/commit/a03978fbe7db82c97b362d5da726886f19d89bf6))
* add order to trace ([#285](https://github.com/gorules/zen/issues/285)) ([f93880b](https://github.com/gorules/zen/commit/f93880b1107517dc2b0430799af51fbce3b0706a))
* add snapshot tests ([#385](https://github.com/gorules/zen/issues/385)) ([4a59f22](https://github.com/gorules/zen/commit/4a59f222db16277f205342bd7a1ebd2ccd058c91))
* bump rquickjs, fix loop cache for decisionTable ([#274](https://github.com/gorules/zen/issues/274)) ([92879f9](https://github.com/gorules/zen/commit/92879f9c05984f961f530b81731ea6c2813ab061))
* compact trace ([#384](https://github.com/gorules/zen/issues/384)) ([80fe402](https://github.com/gorules/zen/commit/80fe4021c216d267bee388a1959a5ae29f279f0d))
* configurable arbitrary precision ([#433](https://github.com/gorules/zen/issues/433)) ([46688a4](https://github.com/gorules/zen/commit/46688a4d4ce72f23db22b4397827b28529a5d71d))
* configurable function timeout ([#367](https://github.com/gorules/zen/issues/367)) ([f36974a](https://github.com/gorules/zen/commit/f36974a8d626ee53e2ba80340218f03c97234441))
* custom node ([#138](https://github.com/gorules/zen/issues/138)) ([daecf90](https://github.com/gorules/zen/commit/daecf901e6576df0ddd9d24dbc2aed6774b4599f))
* date v2 ([#345](https://github.com/gorules/zen/issues/345)) ([de6d13e](https://github.com/gorules/zen/commit/de6d13e1c7f5b5e4b9480f4d0c39b998b208cf0f))
* decision transform attributes ([#255](https://github.com/gorules/zen/issues/255)) ([31d1730](https://github.com/gorules/zen/commit/31d1730f5a3e9e5739e835942cb06a6b88698496))
* evaluation reference map trace; ([#28](https://github.com/gorules/zen/issues/28)) ([69a7947](https://github.com/gorules/zen/commit/69a794785ada249cbeca01310889e61cab201143))
* expose engine methods ([#242](https://github.com/gorules/zen/issues/242)) ([12ca3f4](https://github.com/gorules/zen/commit/12ca3f495116336d7d00189a0fdf2aac6ff9f5de))
* expose expressions through bindings; ([#86](https://github.com/gorules/zen/issues/86)) ([1b0ff9f](https://github.com/gorules/zen/commit/1b0ff9f6f1031d4e8dbced7ba7ebf661e6d91c09))
* expression chain ([#166](https://github.com/gorules/zen/issues/166)) ([6271d8a](https://github.com/gorules/zen/commit/6271d8ab1aa232d0f52c3eb1f513df62650f63ad))
* expression node; ([#34](https://github.com/gorules/zen/issues/34)) ([b713c96](https://github.com/gorules/zen/commit/b713c964be91ee728e2ab75162d80bba733a21ad))
* expression static analysis ([#449](https://github.com/gorules/zen/issues/449)) ([602d214](https://github.com/gorules/zen/commit/602d214b9648964c032cdd2b414b0c40e22c6414))
* function omit nodes ([#370](https://github.com/gorules/zen/issues/370)) ([30b8396](https://github.com/gorules/zen/commit/30b8396f3b3ce13163aa66f8cd0edc6536d2e41d))
* function v2 ([#212](https://github.com/gorules/zen/issues/212)) ([cc3d938](https://github.com/gorules/zen/commit/cc3d938b2f21bda6b66e7c38b0cfc34df239a5c9))
* http auth ([#393](https://github.com/gorules/zen/issues/393)) ([013c131](https://github.com/gorules/zen/commit/013c1312f8fcce73fcd1091d22feb67c7780359c))
* improve switch performance ([#159](https://github.com/gorules/zen/issues/159)) ([2d1beba](https://github.com/gorules/zen/commit/2d1bebab031793eecb25b9b20a276689368ad9f9))
* intellisense ([#244](https://github.com/gorules/zen/issues/244)) ([3e7cdfc](https://github.com/gorules/zen/commit/3e7cdfcde1d44df7b0b34dce40d858ef1be585a4))
* lazy load fn nodes ([#389](https://github.com/gorules/zen/issues/389)) ([07040c8](https://github.com/gorules/zen/commit/07040c8a9607032358c476dea8818f1df509f270))
* mobile support initial ([#399](https://github.com/gorules/zen/issues/399)) ([95154b7](https://github.com/gorules/zen/commit/95154b72f16ed3c5b0129fcbc8d2b4d47b57a3b2))
* optional field for decision model; ([#50](https://github.com/gorules/zen/issues/50)) ([108b768](https://github.com/gorules/zen/commit/108b76868ff0b678099d3db1c1c302ed1281b5a3))
* partial trace ([#287](https://github.com/gorules/zen/issues/287)) ([0f4c522](https://github.com/gorules/zen/commit/0f4c522ee495b834116ab6da20af4b7a65a2546d))
* passthrough nodes ([#261](https://github.com/gorules/zen/issues/261)) ([4781214](https://github.com/gorules/zen/commit/47812143a031e92fedde3b406502f3dc0cfb9dbd))
* policy engine ([#450](https://github.com/gorules/zen/issues/450)) ([6c0ca51](https://github.com/gorules/zen/commit/6c0ca513546dd8cb3664e6926da66f146d9726d9))
* precompile decision content ([#401](https://github.com/gorules/zen/issues/401)) ([7bfebd9](https://github.com/gorules/zen/commit/7bfebd90fed78d3250e25f92a4c971b71f850532))
* rc variable ([#246](https://github.com/gorules/zen/issues/246)) ([9159816](https://github.com/gorules/zen/commit/91598166ce912b8d8f53441d5e9fa8a02bf9855a))
* refactor engine ([#390](https://github.com/gorules/zen/issues/390)) ([9150982](https://github.com/gorules/zen/commit/91509821be632bc7305648d2f6f4ce62f84b4c60))
* replace v8 with quickjs ([#119](https://github.com/gorules/zen/issues/119)) ([c281c55](https://github.com/gorules/zen/commit/c281c55ef13bb751592844327e14de852ca0bf2c))
* schema validation ([#267](https://github.com/gorules/zen/issues/267)) ([2b73856](https://github.com/gorules/zen/commit/2b73856d55067a2dad2af7d6fce8dede507a5f2e))
* switch node ([#103](https://github.com/gorules/zen/issues/103)) ([efcbe36](https://github.com/gorules/zen/commit/efcbe36c1fda9c10c39d4f7e05ecc10f7337f206))
* wasm support ([#404](https://github.com/gorules/zen/issues/404)) ([922a262](https://github.com/gorules/zen/commit/922a262e989ea914e7ed3ccde52f27d31b6b2c39))
* zen expression rewrite ([#107](https://github.com/gorules/zen/issues/107)) ([5f423b7](https://github.com/gorules/zen/commit/5f423b7910feb62d28c84c159705dc8db296d469))


### Bug Fixes

* clean variable before validation ([#409](https://github.com/gorules/zen/issues/409)) ([a6ad0a7](https://github.com/gorules/zen/commit/a6ad0a7894672e73ebf9a228e3fa4fa334e10245))
* decision table json ordering; ([#30](https://github.com/gorules/zen/issues/30)) ([913bc1b](https://github.com/gorules/zen/commit/913bc1bb1cc1b47d1375989cc19ee3134743ad9e))
* DecisionLoader and downcast trait ([#414](https://github.com/gorules/zen/issues/414)) ([8ebc344](https://github.com/gorules/zen/commit/8ebc3446036bafbf6a9a58ea9de2bc2bebaf7613))
* double borrow ([#252](https://github.com/gorules/zen/issues/252)) ([ec6004a](https://github.com/gorules/zen/commit/ec6004a6d4342aa77833df9eb8af72facd8ef323))
* expose graph response ([#104](https://github.com/gorules/zen/issues/104)) ([f3c56b7](https://github.com/gorules/zen/commit/f3c56b7bdb378ed37e16ab5fcb041cd4b3e8a830))
* expression date range; ([#49](https://github.com/gorules/zen/issues/49)) ([9caac0c](https://github.com/gorules/zen/commit/9caac0c8128a894c08b9bdbd2b3bf6ce625bab5e))
* function data pass; ([#80](https://github.com/gorules/zen/issues/80)) ([ca568bc](https://github.com/gorules/zen/commit/ca568bcb4ca6bf6020e663a110ba12c8dbdf58a4))
* function node serde ([#412](https://github.com/gorules/zen/issues/412)) ([1e34717](https://github.com/gorules/zen/commit/1e34717c493abf75826d601643a4dab531a0ff49))
* function trace format ([#288](https://github.com/gorules/zen/issues/288)) ([e940fd6](https://github.com/gorules/zen/commit/e940fd662ddfc56a05e5743e3a315554086f2cf7))
* function v2 quickjs ([#214](https://github.com/gorules/zen/issues/214)) ([4a2312e](https://github.com/gorules/zen/commit/4a2312e9563126fb286f025610ed6b79fa2e32e1))
* graph performance ([#162](https://github.com/gorules/zen/issues/162)) ([bfc8f94](https://github.com/gorules/zen/commit/bfc8f94eaac102eb4194be15ee918c89d6c93645))
* **GRL-517:** bump reqsign to 0.20 to drop vulnerable jsonwebtoken ([#446](https://github.com/gorules/zen/issues/446)) ([63957b7](https://github.com/gorules/zen/commit/63957b701027eca7529d9c25698ccc67538b2fbd))
* http js module breaking ([1d81ce7](https://github.com/gorules/zen/commit/1d81ce78fbdb93a45e6276bd6705a01233edef89))
* http js module breaking on config provided without data (POST) ([1d81ce7](https://github.com/gorules/zen/commit/1d81ce78fbdb93a45e6276bd6705a01233edef89))
* implement serializable errors; improve binding error transparency; ([#62](https://github.com/gorules/zen/issues/62)) ([9277850](https://github.com/gorules/zen/commit/9277850855ecb0ed98df09245af8b32316a31aff))
* improve fn performance ([#416](https://github.com/gorules/zen/issues/416)) ([070a756](https://github.com/gorules/zen/commit/070a7567dbd8f291ec636461486eea6a99054479))
* improve fn variable conversion performance ([#410](https://github.com/gorules/zen/issues/410)) ([de25f98](https://github.com/gorules/zen/commit/de25f98a7f606f5381e2a98a14c1adb9507144d9))
* improve trace ([#333](https://github.com/gorules/zen/issues/333)) ([e10412c](https://github.com/gorules/zen/commit/e10412c2b1e1ae8d0b53920c7d400b735eb5b361))
* improve trace data; ([#57](https://github.com/gorules/zen/issues/57)) ([08131e5](https://github.com/gorules/zen/commit/08131e51984b0df4c0cbe58d5168979207975923))
* napi trace order and performance rounding ([#286](https://github.com/gorules/zen/issues/286)) ([3760feb](https://github.com/gorules/zen/commit/3760feb15c4036eafa0a1b181ca87948fec5853f))
* nested graph reset ([#292](https://github.com/gorules/zen/issues/292)) ([ba288db](https://github.com/gorules/zen/commit/ba288db54116f3a6031b54bef0a6ab8298a8fb04))
* node cache collision ([#295](https://github.com/gorules/zen/issues/295)) ([d6f52f2](https://github.com/gorules/zen/commit/d6f52f29c55f213f6398cb7278f3e8978070de91))
* node merge strategy ([#281](https://github.com/gorules/zen/issues/281)) ([091b2b3](https://github.com/gorules/zen/commit/091b2b3bbcc6f2a79c17feefdb373056a834fc5c))
* omit reserved properties in fn return ([#417](https://github.com/gorules/zen/issues/417)) ([f054aa8](https://github.com/gorules/zen/commit/f054aa876cdff95c5075acb4ba7a3512ebcc5ee8))
* python asyncio ([#217](https://github.com/gorules/zen/issues/217)) ([9e60fdc](https://github.com/gorules/zen/commit/9e60fdc882b979b9a7c28fc51fdf41317d1d4796))
* reference in evaluation ([#363](https://github.com/gorules/zen/issues/363)) ([21a1238](https://github.com/gorules/zen/commit/21a1238efd7137e4ba9173651d4810ce1ca0506c))
* rename templates crate ([#140](https://github.com/gorules/zen/issues/140)) ([ebba323](https://github.com/gorules/zen/commit/ebba3233668779fa510a7d059c027eea43137929))
* resolve root stackoverflow in expression node ([#338](https://github.com/gorules/zen/issues/338)) ([737b9d9](https://github.com/gorules/zen/commit/737b9d9b69946b241a86909e839e9dac97fa5fb0))
* simplify compilation ([#429](https://github.com/gorules/zen/issues/429)) ([6beecb4](https://github.com/gorules/zen/commit/6beecb423f07c3deb25b5f7ff86e042b8f2dc7d0))
* switch node trace ([#224](https://github.com/gorules/zen/issues/224)) ([512aeb6](https://github.com/gorules/zen/commit/512aeb65ba7b38ed4043eb618d2704d3f7d1f4f5))
* trim input before check ([#113](https://github.com/gorules/zen/issues/113)) ([4a6b433](https://github.com/gorules/zen/commit/4a6b4334f20b7ec69c89b35ae8d71a7649fa392f))
* unary reference in string; ([#27](https://github.com/gorules/zen/issues/27)) ([1e883cb](https://github.com/gorules/zen/commit/1e883cb810a8f18df936cfb2ca13a25bbc06050d))
* update dependencies ([#102](https://github.com/gorules/zen/issues/102)) ([20a6856](https://github.com/gorules/zen/commit/20a68564c60f77a91adf3c7df9d54d460b839e1c))
* update quickjs ([#275](https://github.com/gorules/zen/issues/275)) ([f4b6ebb](https://github.com/gorules/zen/commit/f4b6ebbef964e733c0c1d1e732ff3a058af60cfd))
* upgrade crates ([#111](https://github.com/gorules/zen/issues/111)) ([f1f4cb4](https://github.com/gorules/zen/commit/f1f4cb4b08420604963716909c569d4a6fa67c9c))


### Dependencies

* The following workspace dependencies were updated
  * dependencies
    * zen-types bumped from 0.55.1 to 1.0.0-beta.0
    * zen-expression bumped from 0.55.1 to 1.0.0-beta.0
    * zen-tmpl bumped from 0.55.1 to 1.0.0-beta.0
