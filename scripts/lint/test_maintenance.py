"""Offline guards for declared Rust floors and reviewed Action runtimes.

The inventory records immutable upstream revisions whose action manifests were
reviewed as Node 24, composite shell, or Docker. Adding an action requires a
runtime review, not just a moving version tag. These checks do not publish.
"""
import tomllib
import unittest
from pathlib import Path

import yaml

ROOT = Path(__file__).resolve().parents[2]
PINS = {
    # JavaScript actions: runs.using = node24.
    'actions/checkout': 'd23441a48e516b6c34aea4fa41551a30e30af803',
    'actions/upload-artifact': 'b7c566a772e6b6bfb58ed0dc250532a479d7789f',
    'actions/download-artifact': '37930b1c2abaa49bbe596cd826c3c89aef350131',
    'actions/configure-pages': '45bfe0192ca1faeb007ade9deae92b16b8254a0d',
    'actions/deploy-pages': '368f82528645a54fb793d4d04e342629a3f51346',
    'softprops/action-gh-release': 'efb35369e0ad2afab669f228072c1b0d510eae64',
    'Swatinem/rust-cache': '49a0bdc70d2e1b713ca9e2869b211fcce03d3c1c',
    'mozilla-actions/sccache-action': 'fd02668681acd5f960e1372061bee5e3e987195c',
    # Composite shell actions, with no nested JavaScript action.
    'dtolnay/rust-toolchain': '6bed0761d98439e5a578e2877258200ad565ba87',
    'taiki-e/install-action': 'c3ec0de9ae7f1019cea21aa96aa0a895b9552063',
    'canonical/setup-lxd': 'da2ac84e727dc534215fd14cd088b9209d65893b',
    # Docker-based Jekyll build.
    'actions/jekyll-build-pages': '44a6e6beabd48582f863aeeb6cb2151cc1716697',
}


class MaintenanceTests(unittest.TestCase):
    def test_reviewed_action_pins_and_no_legacy_composite_uploader(self):
        """Require the reviewed runtime inventory for every external step action."""
        found = set()
        for path in (ROOT / '.github/workflows').glob('*.yml'):
            source = path.read_text()
            self.assertNotIn('ACTIONS_ALLOW_USE_UNSECURE_NODE_VERSION', source)
            workflow = yaml.safe_load(source)
            for job in workflow['jobs'].values():
                for step in job.get('steps', []):
                    uses = step.get('uses', '')
                    if not uses:
                        continue
                    self.assertIn('@', uses, str(path))
                    action, sha = uses.rsplit('@', 1)
                    self.assertIn(action, PINS, f'{path}: runtime review required for {action}')
                    self.assertEqual(sha, PINS[action], f'{path}: {action}')
                    found.add(action)
        self.assertEqual(found, set(PINS))

    def test_declared_floor_matches_both_os_jobs(self):
        """The six default-feature compiler checks must match package declarations."""
        workflow = yaml.safe_load((ROOT / '.github/workflows/regression.yml').read_text())
        matrix = workflow['jobs']['declared-msrv']['strategy']['matrix']
        self.assertEqual(set(matrix['os']), {'ubuntu-latest', 'windows-latest'})
        self.assertEqual(set(matrix['crate']), {'hyperdu-core', 'hyperdu-gui', 'hyperdu'})
        self.assertEqual({row['crate'] for row in matrix['include']}, set(matrix['crate']))
        for row in matrix['include']:
            package = tomllib.loads((ROOT / row['crate'] / 'Cargo.toml').read_text())['package']
            declared = tuple(map(int, package['rust-version'].split('.')))
            tested = tuple(map(int, row['rust'].split('.')))
            self.assertEqual(declared + (0,) * (3 - len(declared)), tested)
        source = str(workflow['jobs']['declared-msrv'])
        self.assertIn('check --locked', source)
        self.assertIn('--all-targets', source)
        self.assertNotIn('continue-on-error', source)

    def test_pages_keeps_the_expected_tar_artifact_contract(self):
        """Switching the uploader must retain the Pages artifact name and format."""
        jobs = yaml.safe_load((ROOT / '.github/workflows/pages.yml').read_text())['jobs']
        steps = jobs['build']['steps']
        upload = next(s for s in steps if s.get('uses', '').startswith('actions/upload-artifact@'))
        self.assertEqual(upload['with']['name'], 'github-pages')
        self.assertEqual(upload['with']['path'], '${{ runner.temp }}/artifact.tar')
        self.assertEqual(upload['with']['if-no-files-found'], 'error')
        self.assertIn('--dereference --hard-dereference', str(steps))
        self.assertIn("github.event_name != 'pull_request'", jobs['deploy']['if'])

    def test_snap_uses_the_same_helper_without_publication_or_error_suppression(self):
        """The non-publishing Snap check exercises the release helper as a hard gate."""
        release = yaml.safe_load((ROOT / '.github/workflows/release.yml').read_text())
        check = yaml.safe_load((ROOT / '.github/workflows/snap-check.yml').read_text())
        command = 'bash scripts/package/snap-ci.sh'
        self.assertIn(command, str(release['jobs']['linux']))
        self.assertIn(command, str(check['jobs']['build']))
        self.assertEqual(check['permissions'], {'contents': 'read'})
        self.assertNotIn('continue-on-error', str(check['jobs']['build']))
        self.assertNotIn('secrets.', str(check))
        script = (ROOT / 'scripts/package/snap-ci.sh').read_text()
        self.assertIn('snapcraft pack --use-lxd', script)
        self.assertNotRegex(script, r'snapcraft\s+(upload|push|login)\b')
        self.assertIn('github-hosted', script)


if __name__ == '__main__':
    unittest.main()
