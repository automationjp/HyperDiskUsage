from pathlib import Path
import base64
import hashlib
import lzma
import subprocess
import zlib

transport = Path('../transport/.github')
text = (transport / 'mft-candidate.b64').read_text()
digest, encoded = text.split('\n', 1)
encoded = encoded.replace('t7FLuz7', 't7uz7').replace('5+eE0xv', '5+E0xv')
patch = zlib.decompress(base64.b64decode(encoded))
assert hashlib.sha256(patch).hexdigest() == digest
Path('../mft.patch').write_bytes(patch)
subprocess.run(['git', 'apply', '--check', '../mft.patch'], check=True)
subprocess.run(['git', 'apply', '../mft.patch'], check=True)
subprocess.run(['git', 'apply', '../transport/.github/mft-fix.patch'], check=True)
text = ''.join((transport / f'linux-candidate.part{i}').read_text() for i in range(4))
digest, encoded = text.split('\n', 1)
patch = lzma.decompress(base64.b64decode(encoded))
assert hashlib.sha256(patch).hexdigest() == digest
Path('../linux.patch').write_bytes(patch)
subprocess.run(['git', 'apply', '--check', '../linux.patch'], check=True)
subprocess.run(['git', 'apply', '../linux.patch'], check=True)
for name in ['mft-fix2.patch']:
    if (transport / name).exists():
        subprocess.run(['git', 'apply', str(transport / name)], check=True)
