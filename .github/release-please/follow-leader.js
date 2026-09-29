const { registerPlugin } = require('release-please');
const { ManifestPlugin } = require('release-please/build/src/plugin');
const { buildStrategy } = require('release-please/build/src/factory');
const { parseConventionalCommits } = require('release-please/build/src/commit');
const { PatchVersionUpdate } = require('release-please/build/src/versioning-strategy');

class FollowLeader extends ManifestPlugin {
  constructor(github, targetBranch, repositoryConfig, options) {
    super(github, targetBranch, repositoryConfig, options.logger);
    this.leader = options.leader;
    this.followers = new Set(options.followers);
  }

  async preconfigure(strategiesByPath, commitsByPath, releasesByPath) {
    const pathsByComponent = {};
    for (const path in strategiesByPath) {
      pathsByComponent[await strategiesByPath[path].getComponent()] = path;
    }

    const leaderPath = pathsByComponent[this.leader];
    if (!leaderPath) {
      throw new Error(`follow-leader: leader component ${this.leader} not found`);
    }

    const leaderVersion =
      (await this.nextVersion(leaderPath, strategiesByPath, commitsByPath, releasesByPath)) ??
      releasesByPath[leaderPath]?.tag.version;
    if (!leaderVersion) {
      return strategiesByPath;
    }

    const newStrategies = { ...strategiesByPath };
    for (const component of this.followers) {
      const path = pathsByComponent[component];
      if (!path) {
        throw new Error(`follow-leader: follower component ${component} not found`);
      }

      const currentVersion = releasesByPath[path]?.tag.version;
      if (!currentVersion) {
        continue;
      }

      let releaseAs;
      if (compareMinor(leaderVersion, currentVersion) > 0) {
        releaseAs = leaderVersion;
        commitsByPath[path].push({
          sha: '',
          message: `chore(${component}): align with ${this.leader} ${leaderVersion}\n\nRelease-As: ${leaderVersion}`,
        });
      } else {
        const ownVersion = await this.nextVersion(path, strategiesByPath, commitsByPath, releasesByPath);
        if (!ownVersion || compareMinor(ownVersion, currentVersion) === 0) {
          continue;
        }
        releaseAs = new PatchVersionUpdate().bump(currentVersion);
      }

      this.logger.info(`follow-leader: releasing ${component} as ${releaseAs}`);
      newStrategies[path] = await buildStrategy({
        ...this.repositoryConfig[path],
        github: this.github,
        path,
        targetBranch: this.targetBranch,
        releaseAs: releaseAs.toString(),
      });
    }

    return newStrategies;
  }

  async nextVersion(path, strategiesByPath, commitsByPath, releasesByPath) {
    const pullRequest = await strategiesByPath[path].buildReleasePullRequest(
      parseConventionalCommits(commitsByPath[path], this.logger),
      releasesByPath[path],
    );
    return pullRequest?.version;
  }
}

function compareMinor(a, b) {
  return a.major - b.major || a.minor - b.minor;
}

registerPlugin('follow-leader', (options) =>
  new FollowLeader(options.github, options.targetBranch, options.repositoryConfig, options),
);

module.exports = { init: () => {} };
