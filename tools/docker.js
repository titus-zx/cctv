'use strict';
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const {spawnSync} = require('node:child_process');
const root = path.resolve(__dirname, '..');
const data = path.join(root, 'docker-data');
for (const name of ['config', 'recordings', 'rclone']) fs.mkdirSync(path.join(data,name), {recursive:true,mode:0o700});
const runtimeConfig = path.join(data,'config/config.json');
const storageCompose = path.join(data,'storage.compose.json');
if (!fs.existsSync(runtimeConfig)) {
  const source = path.join(root,'config.json');
  const cfg = JSON.parse(fs.readFileSync(fs.existsSync(source) ? source : path.join(root,'config.example.json'),'utf8'));
  const volumes = [];
  if (cfg.recording) {
    cfg.recording.dir = '/app/recordings';
    if (cfg.recording.rclone) {
      delete cfg.recording.rclone.bin; // use the container's rclone, not a host binary
      for (const provider of ['gdrive','onedrive']) {
        const remote = cfg.recording.rclone[provider];
        if (typeof remote === 'string' && path.isAbsolute(remote)) {
          if (!fs.existsSync(remote) || !fs.statSync(remote).isDirectory()) throw new Error(`Folder ${provider} tidak ditemukan: ${remote}`);
          const target = `/app/storage/${provider}`;
          volumes.push({type:'bind',source:remote,target,bind:{create_host_path:false}});
          cfg.recording.rclone[provider] = target;
        }
      }
    }
  }
  fs.writeFileSync(storageCompose,JSON.stringify({services:{viewer:volumes.length ? {volumes} : {}}},null,2)+'\n',{mode:0o600});
  fs.writeFileSync(runtimeConfig,JSON.stringify(cfg,null,2)+'\n',{mode:0o600});
  console.log('Konfigurasi Docker disalin; config.json lokal tetap digunakan oleh npm start.');
}
if (!fs.existsSync(storageCompose)) throw new Error('docker-data/storage.compose.json tidak ditemukan. Pulihkan konfigurasi volume sebelum menjalankan container.');
const envPath = path.join(data,'compose.env');
if (!fs.existsSync(envPath)) fs.writeFileSync(envPath,`LOCAL_UID=${process.getuid?.() ?? 1000}\nLOCAL_GID=${process.getgid?.() ?? 1000}\nVIEWER_PORT=3381\n`,{mode:0o600});
const rcloneConfig = path.join(data,'rclone/rclone.conf');
if (!fs.existsSync(rcloneConfig)) {
  const result = spawnSync(process.env.RCLONE_PATH || 'rclone',['config','file'],{encoding:'utf8'});
  const hostConfig = result.status === 0 ? result.stdout.trim().split('\n').pop() : path.join(os.homedir(),'.config/rclone/rclone.conf');
  if (hostConfig && fs.existsSync(hostConfig)) {
    fs.copyFileSync(hostConfig,rcloneConfig);fs.chmodSync(rcloneConfig,0o600);
    console.log('Konfigurasi rclone disalin untuk container.');
  }
}
const args = process.argv.slice(2);
if (!args.length || args[0] === 'setup') {
  console.log('Siap. Jalankan npm run docker:up; viewer Docker tersedia di http://localhost:3381.');
} else {
  const result = spawnSync('docker',['compose','--env-file',envPath,'-f',path.join(root,'compose.yaml'),'-f',storageCompose,...args],{cwd:root,stdio:'inherit'});
  if (result.error) {console.error(result.error.message);process.exitCode=1;}
  else process.exitCode = result.status ?? 1;
}
