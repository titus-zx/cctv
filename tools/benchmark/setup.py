#!/usr/bin/env python3
"""Prepare isolated benchmark sources/configs without switching branches.

Requires git, Docker and host ffmpeg. Existing private configs are preserved.
"""
import io
import json
import os
import pathlib
import subprocess
import tarfile

ROOT = pathlib.Path(__file__).resolve().parents[2]
WORK = ROOT / '.benchmark'


def run(*args):
    return subprocess.check_output(args, cwd=ROOT)


def main():
    WORK.mkdir(exist_ok=True)
    for name in ('results', 'media', 'config-node', 'config-rust'):
        (WORK / name).mkdir(exist_ok=True)
    arch = json.loads(run('docker', 'info', '--format', '{{json .Architecture}}'))
    arm = arch in ('aarch64', 'arm64')
    rust = WORK / 'rust'
    if not rust.exists():
        rust.mkdir()
        archive = run('git', 'archive', 'origin/rust')
        with tarfile.open(fileobj=io.BytesIO(archive)) as source:
            source.extractall(rust, filter='data')
    # Keep production Dockerfiles intact; adapt copies for native ARM64 builds.
    nodefile = (ROOT / 'Dockerfile').read_text()
    if arm:
        nodefile = nodefile.replace(' intel-media-va-driver', '')
        dockerfile = (rust / 'Dockerfile').read_text()
        if 'x86_64' in dockerfile:
            import re
            dockerfile = dockerfile.replace('x86_64', 'aarch64').replace('X86_64', 'AARCH64')
            dockerfile = dockerfile.replace(' intel-media-va-driver', '')
            dockerfile = re.sub(r'(?<= = ")[a-f0-9]{64}(?=")',
                '26bec5b1e7ba80e0fd65e5d9f4c88c304f04d9c08d2a4da32ab31af8e2fee8eb', dockerfile)
            (rust / 'Dockerfile').write_text(dockerfile)
    (WORK / 'Dockerfile.node').write_text(nodefile)
    (WORK / 'compose.env').write_text(f'LOCAL_UID={os.getuid()}\nLOCAL_GID={os.getgid()}\n')
    source = ROOT / 'docker-data/config/config.json'
    if not source.exists():
        source = ROOT / 'config.json'
    config = json.loads(source.read_text())
    config['port'] = 3380
    config.setdefault('recording', {}).update(enabled=False, retentionDays=0)
    for camera in config['cameras']:
        camera.update(record=False, previewFps=5)
    for version in ('node', 'rust'):
        for path in (WORK / f'config-{version}/config.json', WORK / f'live-{version}.json'):
            if not path.exists():
                path.write_text(json.dumps(config, indent=2) + '\n')
                path.chmod(0o600)
    controlled = {'port': 3380, 'recording': {'enabled': False, 'retentionDays': 0, 'destination': 'local'}, 'cameras': []}
    for n in range(1, 5):
        controlled['cameras'].append(dict(name=f'Replay {n}', ip='172.29.199.10',
            deviceId=str(90000000+n), username='admin', password='benchmark-only',
            mode='v380', quality='hd', audio=False, record=False, previewFps=5, recordingEncoder='cpu'))
    (WORK / 'controlled.json').write_text(json.dumps(controlled, indent=2) + '\n')
    media = WORK / 'media'
    if not (media / 'camera.h264').exists():
        subprocess.run(['ffmpeg', '-v', 'error', '-f', 'lavfi', '-i', 'testsrc2=size=1280x720:rate=16',
            '-t', '8', '-c:v', 'libx264', '-preset', 'veryfast', '-crf', '24', '-pix_fmt', 'yuv420p',
            '-g', '32', '-bf', '0', '-x264-params', 'aud=1', '-f', 'h264', '-y', str(media / 'camera.h264')], check=True)
    if not (media / 'camera.jpg').exists():
        subprocess.run(['ffmpeg', '-v', 'error', '-f', 'lavfi', '-i', 'testsrc2=size=640x480:rate=16',
            '-frames:v', '1', '-c:v', 'mjpeg', '-q:v', '5', '-y', str(media / 'camera.jpg')], check=True)
    print('Prepared .benchmark; live configs have recording/upload disabled.')


if __name__ == '__main__':
    main()
