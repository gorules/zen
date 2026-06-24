# Changelog

## [1.0.0-beta.0](https://github.com/gorules/zen/compare/nodejs-v0.54.0...nodejs-v1.0.0-beta.0) (2026-06-24)


### Features

* add bigjs to function node; ([#77](https://github.com/gorules/zen/issues/77)) ([a03978f](https://github.com/gorules/zen/commit/a03978fbe7db82c97b362d5da726886f19d89bf6))
* add nodejs dispose function ([#146](https://github.com/gorules/zen/issues/146)) ([2a2d0d3](https://github.com/gorules/zen/commit/2a2d0d35d3a3b37df46c72cb7c08aa8785bdf7bd))
* add snapshot tests ([#385](https://github.com/gorules/zen/issues/385)) ([4a59f22](https://github.com/gorules/zen/commit/4a59f222db16277f205342bd7a1ebd2ccd058c91))
* compact trace ([#384](https://github.com/gorules/zen/issues/384)) ([80fe402](https://github.com/gorules/zen/commit/80fe4021c216d267bee388a1959a5ae29f279f0d))
* configurable arbitrary precision ([#433](https://github.com/gorules/zen/issues/433)) ([46688a4](https://github.com/gorules/zen/commit/46688a4d4ce72f23db22b4397827b28529a5d71d))
* configurable function timeout ([#367](https://github.com/gorules/zen/issues/367)) ([f36974a](https://github.com/gorules/zen/commit/f36974a8d626ee53e2ba80340218f03c97234441))
* custom node ([#138](https://github.com/gorules/zen/issues/138)) ([daecf90](https://github.com/gorules/zen/commit/daecf901e6576df0ddd9d24dbc2aed6774b4599f))
* expose expressions through bindings; ([#86](https://github.com/gorules/zen/issues/86)) ([1b0ff9f](https://github.com/gorules/zen/commit/1b0ff9f6f1031d4e8dbced7ba7ebf661e6d91c09))
* function v2 ([#212](https://github.com/gorules/zen/issues/212)) ([cc3d938](https://github.com/gorules/zen/commit/cc3d938b2f21bda6b66e7c38b0cfc34df239a5c9))
* http auth ([#393](https://github.com/gorules/zen/issues/393)) ([013c131](https://github.com/gorules/zen/commit/013c1312f8fcce73fcd1091d22feb67c7780359c))
* improve nodejs bindings ([#145](https://github.com/gorules/zen/issues/145)) ([921caeb](https://github.com/gorules/zen/commit/921caeb4973ebce85b84eb692ae8eabf642f0fc0))
* improve switch performance ([#159](https://github.com/gorules/zen/issues/159)) ([2d1beba](https://github.com/gorules/zen/commit/2d1bebab031793eecb25b9b20a276689368ad9f9))
* musl builds for Node.js ([#277](https://github.com/gorules/zen/issues/277)) ([320de1b](https://github.com/gorules/zen/commit/320de1bc770b2af18d4234f6fed76f56ac223ef1))
* passthrough nodes ([#261](https://github.com/gorules/zen/issues/261)) ([4781214](https://github.com/gorules/zen/commit/47812143a031e92fedde3b406502f3dc0cfb9dbd))
* policy engine ([#450](https://github.com/gorules/zen/issues/450)) ([6c0ca51](https://github.com/gorules/zen/commit/6c0ca513546dd8cb3664e6926da66f146d9726d9))
* precompile decision content ([#401](https://github.com/gorules/zen/issues/401)) ([7bfebd9](https://github.com/gorules/zen/commit/7bfebd90fed78d3250e25f92a4c971b71f850532))
* rc variable ([#246](https://github.com/gorules/zen/issues/246)) ([9159816](https://github.com/gorules/zen/commit/91598166ce912b8d8f53441d5e9fa8a02bf9855a))
* refactor engine ([#390](https://github.com/gorules/zen/issues/390)) ([9150982](https://github.com/gorules/zen/commit/91509821be632bc7305648d2f6f4ce62f84b4c60))
* replace v8 with quickjs ([#119](https://github.com/gorules/zen/issues/119)) ([c281c55](https://github.com/gorules/zen/commit/c281c55ef13bb751592844327e14de852ca0bf2c))
* safe nodejs api ([#148](https://github.com/gorules/zen/issues/148)) ([32d49a0](https://github.com/gorules/zen/commit/32d49a06f35cb3cd0704d6c05c33847551798abe))
* wasm support ([#404](https://github.com/gorules/zen/issues/404)) ([922a262](https://github.com/gorules/zen/commit/922a262e989ea914e7ed3ccde52f27d31b6b2c39))
* zen expression rewrite ([#107](https://github.com/gorules/zen/issues/107)) ([5f423b7](https://github.com/gorules/zen/commit/5f423b7910feb62d28c84c159705dc8db296d469))


### Bug Fixes

* change nodejs versioning system ([#342](https://github.com/gorules/zen/issues/342)) ([f9d9b13](https://github.com/gorules/zen/commit/f9d9b1342ce73f8d19ba403050096a7e3de0098e))
* doc update ([#120](https://github.com/gorules/zen/issues/120)) ([90ad94d](https://github.com/gorules/zen/commit/90ad94dfa7137442d11127290f68dda1edfd2dd4))
* docs ([#122](https://github.com/gorules/zen/issues/122)) ([64cd27a](https://github.com/gorules/zen/commit/64cd27ac1add38e52c7c2aba6f27a19ac5dbac9e))
* **GRL-189:** critical form-data vulnerability ([#381](https://github.com/gorules/zen/issues/381)) ([a2228b1](https://github.com/gorules/zen/commit/a2228b1daa039159c80a79aa23b5a28aafcdc1ef))
* implement serializable errors; improve binding error transparency; ([#62](https://github.com/gorules/zen/issues/62)) ([9277850](https://github.com/gorules/zen/commit/9277850855ecb0ed98df09245af8b32316a31aff))
* napi open handle ([#40](https://github.com/gorules/zen/issues/40)) ([adaf3f0](https://github.com/gorules/zen/commit/adaf3f0b28ffe15a692137dd03016c250cd32ed1))
* napi trace order and performance rounding ([#286](https://github.com/gorules/zen/issues/286)) ([3760feb](https://github.com/gorules/zen/commit/3760feb15c4036eafa0a1b181ca87948fec5853f))
* node type ([#79](https://github.com/gorules/zen/issues/79)) ([bc08017](https://github.com/gorules/zen/commit/bc08017afae0033a6e386f3d74636d27f11e0574))
* nodejs add dispose method ([#413](https://github.com/gorules/zen/issues/413)) ([bb70da3](https://github.com/gorules/zen/commit/bb70da3447dec2afba270cd605857b429d80810a))
* nodejs bindings napi migration to v3 ([a97e82a](https://github.com/gorules/zen/commit/a97e82aab2be6ce7bc231de69e8ace5e6f0613ef))
* nodejs versioning ([#344](https://github.com/gorules/zen/issues/344)) ([f0a3c75](https://github.com/gorules/zen/commit/f0a3c7512999fdd7377cd40c857a5c0765aff47b))
* publish packages ([83c73df](https://github.com/gorules/zen/commit/83c73df5a99e159c4ece3087abe23c58911bd90d))
* rename templates crate ([#140](https://github.com/gorules/zen/issues/140)) ([ebba323](https://github.com/gorules/zen/commit/ebba3233668779fa510a7d059c027eea43137929))
* ts evaluate options; ([#37](https://github.com/gorules/zen/issues/37)) ([23097d0](https://github.com/gorules/zen/commit/23097d0dd8fd4025ef4cf9f082352fc055ddcc1f))
* update dependencies ([#102](https://github.com/gorules/zen/issues/102)) ([20a6856](https://github.com/gorules/zen/commit/20a68564c60f77a91adf3c7df9d54d460b839e1c))
* update packages access mode ([#278](https://github.com/gorules/zen/issues/278)) ([16f318e](https://github.com/gorules/zen/commit/16f318e0d296cc4bf823f845512e58d402632f52))
* upgrade crates ([#111](https://github.com/gorules/zen/issues/111)) ([f1f4cb4](https://github.com/gorules/zen/commit/f1f4cb4b08420604963716909c569d4a6fa67c9c))
* wrap async calls in promise ([#415](https://github.com/gorules/zen/issues/415)) ([0175637](https://github.com/gorules/zen/commit/0175637b1f916be5f1a54e536ddca32bcbf9903b))
