module.exports = {
  testEnvironment: 'node',
  testTimeout: 60000,
  transform: {
    '^.+\\.tsx?$': 'ts-jest',
    '^.+\\.m?js$': ['ts-jest', { tsconfig: { allowJs: true, module: 'commonjs', target: 'es2022' }, isolatedModules: true }],
  },
  transformIgnorePatterns: ['/node_modules/(?!(@napi-rs|@emnapi|@tybys)/)'],
};
