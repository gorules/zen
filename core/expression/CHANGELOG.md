# Changelog

## [1.0.0-beta.2](https://github.com/gorules/zen/compare/zen-expression-v1.0.0-beta.1...zen-expression-v1.0.0-beta.2) (2026-06-26)


### Miscellaneous

* **zen-expression:** Synchronize core versions

## [1.0.0-beta.1](https://github.com/gorules/zen/compare/zen-expression-v1.0.0-beta.0...zen-expression-v1.0.0-beta.1) (2026-06-25)


### Miscellaneous

* **zen-expression:** Synchronize core versions


### Dependencies

* The following workspace dependencies were updated
  * dependencies
    * zen-macros bumped from 1.0.0-beta.0 to 1.0.0-beta.1
    * zen-types bumped from 1.0.0-beta.0 to 1.0.0-beta.1

## [1.0.0-beta.0](https://github.com/gorules/zen/compare/zen-expression-v0.55.1...zen-expression-v1.0.0-beta.0) (2026-06-25)


### Features

* add bigjs to function node; ([#77](https://github.com/gorules/zen/issues/77)) ([a03978f](https://github.com/gorules/zen/commit/a03978fbe7db82c97b362d5da726886f19d89bf6))
* add regular expression functions; ([#52](https://github.com/gorules/zen/issues/52)) ([fe7ba69](https://github.com/gorules/zen/commit/fe7ba6996d42f10ed7b2fd91b7735c8283f7a806))
* add snapshot tests ([#385](https://github.com/gorules/zen/issues/385)) ([4a59f22](https://github.com/gorules/zen/commit/4a59f222db16277f205342bd7a1ebd2ccd058c91))
* add trim function to string data type ([#279](https://github.com/gorules/zen/issues/279)) ([49814e1](https://github.com/gorules/zen/commit/49814e1aa0a284675f1c26c2bd403575145354dd))
* add validation methods to python bindings ([#305](https://github.com/gorules/zen/issues/305)) ([94e1da2](https://github.com/gorules/zen/commit/94e1da239af1a89e9de4c6687460a3fac5fa5dec))
* additional math functions; ([#51](https://github.com/gorules/zen/issues/51)) ([0212fe1](https://github.com/gorules/zen/commit/0212fe1a0f6f345880df5474d166d706082b86a7))
* assignment ([#366](https://github.com/gorules/zen/issues/366)) ([f2fcb95](https://github.com/gorules/zen/commit/f2fcb95ad90fdecf4ff12f8969dc0cf1ed86e6bf))
* bump rquickjs, fix loop cache for decisionTable ([#274](https://github.com/gorules/zen/issues/274)) ([92879f9](https://github.com/gorules/zen/commit/92879f9c05984f961f530b81731ea6c2813ab061))
* compact trace ([#384](https://github.com/gorules/zen/issues/384)) ([80fe402](https://github.com/gorules/zen/commit/80fe4021c216d267bee388a1959a5ae29f279f0d))
* compiled bytecode ([#307](https://github.com/gorules/zen/issues/307)) ([ae40aff](https://github.com/gorules/zen/commit/ae40aff1638bde0011954ba199b831657f155928))
* configurable arbitrary precision ([#433](https://github.com/gorules/zen/issues/433)) ([46688a4](https://github.com/gorules/zen/commit/46688a4d4ce72f23db22b4397827b28529a5d71d))
* custom node ([#138](https://github.com/gorules/zen/issues/138)) ([daecf90](https://github.com/gorules/zen/commit/daecf901e6576df0ddd9d24dbc2aed6774b4599f))
* date functions; ([#91](https://github.com/gorules/zen/issues/91)) ([78d8d26](https://github.com/gorules/zen/commit/78d8d2632adfbc57dea603d448f22d5ac1e79287))
* date v2 ([#345](https://github.com/gorules/zen/issues/345)) ([de6d13e](https://github.com/gorules/zen/commit/de6d13e1c7f5b5e4b9480f4d0c39b998b208cf0f))
* dates min/max ([#353](https://github.com/gorules/zen/issues/353)) ([34de232](https://github.com/gorules/zen/commit/34de232d3abcc6d81f1993b4e870125c80a6f6e9))
* deserialize variable type ([#256](https://github.com/gorules/zen/issues/256)) ([7b861d0](https://github.com/gorules/zen/commit/7b861d0642bfecd0af927ae072af9560a91f8873))
* evaluation reference map trace; ([#28](https://github.com/gorules/zen/issues/28)) ([69a7947](https://github.com/gorules/zen/commit/69a794785ada249cbeca01310889e61cab201143))
* expose expressions through bindings; ([#86](https://github.com/gorules/zen/issues/86)) ([1b0ff9f](https://github.com/gorules/zen/commit/1b0ff9f6f1031d4e8dbced7ba7ebf661e6d91c09))
* expression chain ([#166](https://github.com/gorules/zen/issues/166)) ([6271d8a](https://github.com/gorules/zen/commit/6271d8ab1aa232d0f52c3eb1f513df62650f63ad))
* expression function system ([#340](https://github.com/gorules/zen/issues/340)) ([6535d44](https://github.com/gorules/zen/commit/6535d44e0329138b0df11236a7f042a0a92527df))
* expression static analysis ([#449](https://github.com/gorules/zen/issues/449)) ([602d214](https://github.com/gorules/zen/commit/602d214b9648964c032cdd2b414b0c40e22c6414))
* function scope alias ([#426](https://github.com/gorules/zen/issues/426)) ([73d5d5c](https://github.com/gorules/zen/commit/73d5d5c6cb7fb2cf87623dc6aaf63eefd774431c))
* fuzzy match ([#168](https://github.com/gorules/zen/issues/168)) ([7ce6ceb](https://github.com/gorules/zen/commit/7ce6ceb9e448e499673ddffd7e0828ceda1a69cc))
* implement new functions - part 1 ([#205](https://github.com/gorules/zen/issues/205)) ([e452df3](https://github.com/gorules/zen/commit/e452df33e9c7ef7f5069f9b0a34b8074478e4c35))
* improve stack resiliency ([#357](https://github.com/gorules/zen/issues/357)) ([173d7bc](https://github.com/gorules/zen/commit/173d7bcb7956a4b4c66173d7cbec3e3a4d4386c4))
* intellisense ([#244](https://github.com/gorules/zen/issues/244)) ([3e7cdfc](https://github.com/gorules/zen/commit/3e7cdfcde1d44df7b0b34dce40d858ef1be585a4))
* interval iterator ([#336](https://github.com/gorules/zen/issues/336)) ([36c3281](https://github.com/gorules/zen/commit/36c32814b8b8b1875b01a2326d44f55b7fc1e814))
* is numeric function; ([#95](https://github.com/gorules/zen/issues/95)) ([807537a](https://github.com/gorules/zen/commit/807537ad77ec04f6efa406994a5166ec8a67afee))
* keys function ([#135](https://github.com/gorules/zen/issues/135)) ([ae68b4a](https://github.com/gorules/zen/commit/ae68b4a0dd81491145d957a6e8282bbce3a3ccde))
* merge function ([#438](https://github.com/gorules/zen/issues/438)) ([53f3b5a](https://github.com/gorules/zen/commit/53f3b5a430a6c1b907e7445acd08a80f9b14214f))
* number scientific notation ([#355](https://github.com/gorules/zen/issues/355)) ([29ce060](https://github.com/gorules/zen/commit/29ce060ac78759106fec2db32df6f84a17929723))
* object support ([#226](https://github.com/gorules/zen/issues/226)) ([c9d1e9c](https://github.com/gorules/zen/commit/c9d1e9cc32d50508808f4ae0d1683445518beb30))
* optional field for decision model; ([#50](https://github.com/gorules/zen/issues/50)) ([108b768](https://github.com/gorules/zen/commit/108b76868ff0b678099d3db1c1c302ed1281b5a3))
* optional lite regex ([#84](https://github.com/gorules/zen/issues/84)) ([19d529f](https://github.com/gorules/zen/commit/19d529ff89dba2be85431e0db921440037f80fea))
* passthrough nodes ([#261](https://github.com/gorules/zen/issues/261)) ([4781214](https://github.com/gorules/zen/commit/47812143a031e92fedde3b406502f3dc0cfb9dbd))
* precompile decision content ([#401](https://github.com/gorules/zen/issues/401)) ([7bfebd9](https://github.com/gorules/zen/commit/7bfebd90fed78d3250e25f92a4c971b71f850532))
* rc variable ([#246](https://github.com/gorules/zen/issues/246)) ([9159816](https://github.com/gorules/zen/commit/91598166ce912b8d8f53441d5e9fa8a02bf9855a))
* refactor engine ([#390](https://github.com/gorules/zen/issues/390)) ([9150982](https://github.com/gorules/zen/commit/91509821be632bc7305648d2f6f4ce62f84b4c60))
* standardise operators; ([#65](https://github.com/gorules/zen/issues/65)) ([e4b2691](https://github.com/gorules/zen/commit/e4b2691a06d43096f3bad7528194eb6fc7958cf1))
* template string ([#225](https://github.com/gorules/zen/issues/225)) ([7cb676d](https://github.com/gorules/zen/commit/7cb676d36a05032f85e7554daacb37e7ae2f4026))
* time function ([#31](https://github.com/gorules/zen/issues/31)) ([bd419e6](https://github.com/gorules/zen/commit/bd419e6e492e624b6881b1bb92f9bce6a4f2b60f))
* trunc function and decimal places for round (optional) ([#349](https://github.com/gorules/zen/issues/349)) ([d179552](https://github.com/gorules/zen/commit/d179552b3ca521cd7bc91d000218bd65bef814b7))
* type conversions; ([#94](https://github.com/gorules/zen/issues/94)) ([7b36527](https://github.com/gorules/zen/commit/7b36527dc7a1f01b5aa1105bad210d948bc78b7b))
* upgrade dependencies ([#240](https://github.com/gorules/zen/issues/240)) ([719d43a](https://github.com/gorules/zen/commit/719d43a26d59ba9d6667171a71359294aea21c20))
* wasm support ([#404](https://github.com/gorules/zen/issues/404)) ([922a262](https://github.com/gorules/zen/commit/922a262e989ea914e7ed3ccde52f27d31b6b2c39))
* zen expression rewrite ([#107](https://github.com/gorules/zen/issues/107)) ([5f423b7](https://github.com/gorules/zen/commit/5f423b7910feb62d28c84c159705dc8db296d469))


### Bug Fixes

* add operator typecheck ([#250](https://github.com/gorules/zen/issues/250)) ([9d0ef40](https://github.com/gorules/zen/commit/9d0ef4060b5fd8ee6d58a2f4c4224e6439865719))
* align date diff with dayjs ([#360](https://github.com/gorules/zen/issues/360)) ([a911a62](https://github.com/gorules/zen/commit/a911a623cbc119ae6a62ef6797c32d94df256e94))
* correctly throw an error when closed bracket is missing in function call ([#337](https://github.com/gorules/zen/issues/337)) ([f81d118](https://github.com/gorules/zen/commit/f81d1188f6f55af4893b88e30bc6861a04f95df6))
* date endOf for a year and isAfter comparison for units ([#348](https://github.com/gorules/zen/issues/348)) ([6027f5a](https://github.com/gorules/zen/commit/6027f5a6a15d1a224f75c69113379bba1a6bc0b2))
* date fn type ([#245](https://github.com/gorules/zen/issues/245)) ([9fba1ff](https://github.com/gorules/zen/commit/9fba1ff7be54e64aa2c195d03d09363979f5b762))
* date method signature ([#351](https://github.com/gorules/zen/issues/351)) ([e09f2ed](https://github.com/gorules/zen/commit/e09f2ede5e41dad018810cdd2266895c1150c050))
* double borrow ([#252](https://github.com/gorules/zen/issues/252)) ([ec6004a](https://github.com/gorules/zen/commit/ec6004a6d4342aa77833df9eb8af72facd8ef323))
* dual arity argument parsing; ([#55](https://github.com/gorules/zen/issues/55)) ([404e91d](https://github.com/gorules/zen/commit/404e91dc59d2a2b6d45ea8580f04f01656f3827f))
* empty object ([#241](https://github.com/gorules/zen/issues/241)) ([a903982](https://github.com/gorules/zen/commit/a903982e17812b361a77555597c506bb20657f5f))
* expression date range; ([#49](https://github.com/gorules/zen/issues/49)) ([9caac0c](https://github.com/gorules/zen/commit/9caac0c8128a894c08b9bdbd2b3bf6ce625bab5e))
* improve fn variable conversion performance ([#410](https://github.com/gorules/zen/issues/410)) ([de25f98](https://github.com/gorules/zen/commit/de25f98a7f606f5381e2a98a14c1adb9507144d9))
* improve type errors ([#259](https://github.com/gorules/zen/issues/259)) ([5a37ef3](https://github.com/gorules/zen/commit/5a37ef319d092c37d6823d95218715383ccee60c))
* lossy exponent fallback ([#352](https://github.com/gorules/zen/issues/352)) ([157859c](https://github.com/gorules/zen/commit/157859ca3709b33caa0aca23c83608d293b04a2d))
* negative value checks ([#89](https://github.com/gorules/zen/issues/89)) ([7b17575](https://github.com/gorules/zen/commit/7b17575a6681ea79b17c5a34aa2838d2ea313158))
* normalize number during serialization ([#359](https://github.com/gorules/zen/issues/359)) ([c9d11f1](https://github.com/gorules/zen/commit/c9d11f12dd7863a3602059c730adf3eea8f02c93))
* number fn type ([#354](https://github.com/gorules/zen/issues/354)) ([71b949b](https://github.com/gorules/zen/commit/71b949b1e1d173b3b1be25459d88ffc87e679ab0))
* number serde ([#248](https://github.com/gorules/zen/issues/248)) ([6c85cbc](https://github.com/gorules/zen/commit/6c85cbc04dc6b34c02e5e705f6793e0db49f7f4f))
* number serialization ([#247](https://github.com/gorules/zen/issues/247)) ([9a87367](https://github.com/gorules/zen/commit/9a873675db72c8dd13499884b18c4dba9f1b9422))
* py validate methods ([#316](https://github.com/gorules/zen/issues/316)) ([d762431](https://github.com/gorules/zen/commit/d7624311f60df5c262ed0cd5bd273e1f2362b2c7))
* remove missing date fns; ([#71](https://github.com/gorules/zen/issues/71)) ([7283047](https://github.com/gorules/zen/commit/7283047f490d8f596f3fa85d1cb5dbf6b3c2b697))
* rust version bump ([#387](https://github.com/gorules/zen/issues/387)) ([1e4135f](https://github.com/gorules/zen/commit/1e4135f5fb665d21237054501ac6d671e1b55e64))
* safe division ([#365](https://github.com/gorules/zen/issues/365)) ([bdf51de](https://github.com/gorules/zen/commit/bdf51dec1104b56f202b17084379b8c3d3610ab6))
* simplify compilation ([#429](https://github.com/gorules/zen/issues/429)) ([6beecb4](https://github.com/gorules/zen/commit/6beecb423f07c3deb25b5f7ff86e042b8f2dc7d0))
* string slices; remove int variable type; ([#92](https://github.com/gorules/zen/issues/92)) ([615d554](https://github.com/gorules/zen/commit/615d554215357eff8ccbe484cc5add765a8e8aba))
* timezone caching ([#371](https://github.com/gorules/zen/issues/371)) ([09f59e9](https://github.com/gorules/zen/commit/09f59e9bb2c6be6400cffcec199569395ff7e5db))
* unary closure expression ([#301](https://github.com/gorules/zen/issues/301)) ([df2a44f](https://github.com/gorules/zen/commit/df2a44f8b38eee19ffbdf9b59fafd4594680a61e))
* unary reference in string; ([#27](https://github.com/gorules/zen/issues/27)) ([1e883cb](https://github.com/gorules/zen/commit/1e883cb810a8f18df936cfb2ca13a25bbc06050d))
* unterminated statement ([#29](https://github.com/gorules/zen/issues/29)) ([9abfb0c](https://github.com/gorules/zen/commit/9abfb0c4ba1bf086edd07d268d6a12c989955dbb))
* update rounding strategy ([#361](https://github.com/gorules/zen/issues/361)) ([815729a](https://github.com/gorules/zen/commit/815729aca47f275692695203b12576e96e750490))
* upgrade crates ([#111](https://github.com/gorules/zen/issues/111)) ([f1f4cb4](https://github.com/gorules/zen/commit/f1f4cb4b08420604963716909c569d4a6fa67c9c))


### Dependencies

* The following workspace dependencies were updated
  * dependencies
    * zen-macros bumped from 0.55.1 to 1.0.0-beta.0
    * zen-types bumped from 0.55.1 to 1.0.0-beta.0
