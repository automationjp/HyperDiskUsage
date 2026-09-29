"""Offline guards for declared Rust floors and reviewed Node 24 action pins."""
import tomllib
import unittest
from pathlib import Path

import yaml

ROOT = Path(__file__).resolve().parents[2]
PINS = {
    'actions/checkout': 'd23441a48e516b6c34aea4fa41551a30e30af803',
    'actions/upload-artifact': 'b7c566a772e6b6bfb58ed0dc250532a479d7789f',
    'actions/download-artifact': '37930b1c2abaa49bbe596cd826c3c89aef350131',
    'actions/configure-pages': '45bfe0192ca1faeb007ade9deae92b16b8254a0d',
    'actions/deploy-pages': '368f82528645a54fb793d4d04e342629a3f51346',
    'softprops/action-gh-release': 'efb35369e0ad2afab669f228072c1b0d510eae64',
}

class MaintenanceTests(unittest.TestCase):
    def test_reviewed_action_pins_and_no_legacy_composite_uploader(self):
        found = set()
        for path in (ROOT / '.github/workflows').glob('*.yml'):
            source = path.read_text()
            self.assertNotIn('ACTIONS_ALLOW_USE_UNSECURE_NODE_VERSION', source)
            workflow = yaml.safe_load(source)
            for job in workflow['jobs'].values():
                for step in job.get('steps', []):
                    uses = step.get('uses', '')
                    if '@' not in uses:
                        continue
                    action, sha = uses.rsplit('@', 1)
                    self.assertNotEqual(action, 'actions/upload-pages-artifact')
                    if action in PINS:
                        self.assertEqual(sha, PINS[action], f'{path}: {action}')
                        found.add(action)
        self.assertEqual(found, set(PINS))

    def test_declared_floor_matches_both_os_jobs(self):
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
        jobs = yaml.safe_load((ROOT / '.github/workflows/pages.yml').read_text())['jobs']
        steps = jobs['build']['steps']
        upload = next(s for s in steps if s.get('uses', '').startswith('actions/upload-artifact@'))
        self.assertEqual(upload['with']['name'], 'github-pages')
        self.assertEqual(upload['with']['path'], '${{ runner.temp }}/artifact.tar')
        self.assertEqual(upload['with']['if-no-files-found'], 'error')
        self.assertIn('--dereference --hard-dereference', str(steps))
        self.assertIn("github.event_name != 'pull_request'", jobs['deploy']['if'])

if __name__ == '__main__':
    unittest.main()
