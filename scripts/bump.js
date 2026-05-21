const fs = require('fs');
const path = require('path');
const readline = require('readline');

const mainCratePath = path.join(__dirname, '..', 'crates', 'cli', 'Cargo.toml');
const manifest = fs.readFileSync(mainCratePath, 'utf8');
const versionMatch = manifest.match(/^version = "(.*)"/m);

if (!versionMatch) {
  console.error('Could not find version in crates/cli/Cargo.toml');
  process.exit(1);
}

const currentVersion = versionMatch[1];
// Semver regex: major.minor.patch[-prerelease]
const semverRegex = /^(\d+)\.(\d+)\.(\d+)(?:-([\w\d.-]+))?$/;
const match = currentVersion.match(semverRegex);

if (!match) {
  console.error(`Invalid semver version in Cargo.toml: ${currentVersion}`);
  process.exit(1);
}

let [_, major, minor, patch, pre] = match;
major = Number(major);
minor = Number(minor);
patch = Number(patch);

function getNextPre(label) {
  if (pre && pre.startsWith(label)) {
    const parts = pre.split('.');
    const num = parts.length > 1 ? Number(parts[1]) : 0;
    return `${major}.${minor}.${patch}-${label}.${num + 1}`;
  }
  return `${major}.${minor}.${patch + 1}-${label}.1`;
}

async function run() {
  let mode = process.argv[2];

  if (!mode) {
    const rl = readline.createInterface({
      input: process.stdin,
      output: process.stdout
    });

    console.log(`\x1b[36mKumo Version Bumper\x1b[0m (Current: \x1b[33m${currentVersion}\x1b[0m)`);
    console.log('Select bump type:');
    console.log(`1) Patch  (\x1b[32m${major}.${minor}.${pre ? patch : patch + 1}\x1b[0m)${pre ? ' (Stable release)' : ''}`);
    console.log(`2) Minor  (\x1b[32m${major}.${minor + 1}.0\x1b[0m)`);
    console.log(`3) Major  (\x1b[32m${major + 1}.0.0\x1b[0m)`);
    console.log(`4) Alpha  (\x1b[32m${getNextPre('alpha')}\x1b[0m) - \x1b[90mInternal testing, unstable\x1b[0m`);
    console.log(`5) Beta   (\x1b[32m${getNextPre('beta')}\x1b[0m) - \x1b[90mFeature complete, public testing\x1b[0m`);
    console.log(`6) RC     (\x1b[32m${getNextPre('rc')}\x1b[0m) - \x1b[90mRelease Candidate, potential final\x1b[0m`);
    console.log('7) Custom Version');

    const choice = await new Promise(resolve => rl.question('Choice: ', resolve));

    if (choice === '1') mode = 'patch';
    else if (choice === '2') mode = 'minor';
    else if (choice === '3') mode = 'major';
    else if (choice === '4') mode = 'alpha';
    else if (choice === '5') mode = 'beta';
    else if (choice === '6') mode = 'rc';
    else if (choice === '7') {
      mode = await new Promise(resolve => rl.question('Enter version: ', resolve));
    } else {
      console.log('Invalid choice.');
      process.exit(1);
    }
    rl.close();
  }

  let newVersion;
  switch (mode) {
    case 'major': newVersion = `${major + 1}.0.0`; break;
    case 'minor': newVersion = `${major}.${minor + 1}.0`; break;
    case 'patch':
      newVersion = pre ? `${major}.${minor}.${patch}` : `${major}.${minor}.${patch + 1}`;
      break;
    case 'alpha': newVersion = getNextPre('alpha'); break;
    case 'beta': newVersion = getNextPre('beta'); break;
    case 'rc': newVersion = getNextPre('rc'); break;
    default:
      newVersion = mode.startsWith('v') ? mode.slice(1) : mode;
  }

  console.log(`Bumping version: \x1b[33m${currentVersion}\x1b[0m -> \x1b[32m${newVersion}\x1b[0m`);

  const cratesDir = path.join(__dirname, '..', 'crates');
  const crates = fs.readdirSync(cratesDir);

  crates.forEach(crate => {
    const cargoPath = path.join(cratesDir, crate, 'Cargo.toml');
    if (fs.existsSync(cargoPath)) {
      console.log(`  Updating ${crate}...`);
      let content = fs.readFileSync(cargoPath, 'utf8');
      content = content.replace(/^version = ".*"/m, `version = "${newVersion}"`);
      fs.writeFileSync(cargoPath, content);
    }
  });

  console.log('\x1b[32mVersion update complete!\x1b[0m');

  try {
    const { execSync } = require('child_process');
    console.log('Committing version bump...');

    const isPreRelease = newVersion.includes('-');
    const targetBranch = isPreRelease ? 'dev' : 'master';

    execSync('git add .');
    execSync(`git commit -m "release: v${newVersion}"`);

    console.log(`Pulling latest changes from origin/${targetBranch} with rebase...`);
    execSync(`git pull --rebase origin ${targetBranch}`);

    console.log(`Tagging v${newVersion}...`);
    execSync(`git tag -f v${newVersion}`);

    console.log(`Pushing changes and tag v${newVersion} to origin/\x1b[35m${targetBranch}\x1b[0m...`);
    // Push the current HEAD to the target branch
    execSync(`git push origin HEAD:${targetBranch}`);

    // Push the tag
    execSync(`git push origin v${newVersion} --force`);

    if (targetBranch === "master") {
      // push dev too, because it's importante maintain the dev branch updated.
      console.log('Updating dev branch...');
      try {
        execSync(`git push origin dev`);
        console.log(`Pushed changes to dev branch.`);
      } catch (devError) {
        console.warn('Failed to push to dev branch, you may need to sync it manually:', devError.message);
      }
    }

    console.log(`\n\x1b[32mSuccessfully released v${newVersion}!\x1b[0m`);
    console.log(`Code pushed to \x1b[35m${targetBranch}\x1b[0m branch.`);
    console.log('GitHub Actions will now build the artifacts.');
  } catch (error) {
    console.error('Detailed error log:', error);
    console.warn('\x1b[31mGit operations failed.\x1b[0m Please make sure you have git installed and are in a git repository.');
    console.log(`Manual steps needed:\n   git commit -am "release: v${newVersion}"\n   git tag v${newVersion}\n   git push origin v${newVersion}`);
  }
}

run();
