'use strict';
const crypto = require('node:crypto');
const net = require('node:net');

// V380 stream dispatch protocol: https://github.com/PyanSofyan/V380Decoder
const DISPATCH_URL = 'http://dispa1.av380.net:8001/api/v1/get_stream_server';
function dispatchRequest(deviceId, timestamp = Math.floor(Date.now() / 1000)) {
  const dev_id = Number(deviceId), platform = 10001;
  if (!Number.isInteger(dev_id) || dev_id < 1 || dev_id > 0xffffffff) throw new Error('Device ID tidak valid.');
  const sign = crypto.createHash('sha1')
    .update(`dev_id=${dev_id}&platform=${platform}&timestamp=${timestamp}hsdata2022`).digest('hex');
  return { dev_id, platform, timestamp, sign };
}
function reachable(host, port, signal) {
  return new Promise(resolve => {
    const socket = new net.Socket();
    let settled = false;
    const finish = result => {
      if (settled) return;
      settled = true; signal?.removeEventListener('abort', abort); socket.destroy(); resolve(result);
    };
    const abort = () => finish(false);
    socket.on('error', () => finish(false));
    socket.on('timeout', () => finish(false));
    socket.once('connect', () => finish(true));
    signal?.addEventListener('abort', abort, { once: true });
    if (signal?.aborted) return finish(false);
    socket.setTimeout(2500); socket.connect(port, host);
  });
}
async function resolveRelay(deviceId, {signal, request = fetch, probe = reachable} = {}) {
  const controller = new AbortController();
  const abort = () => controller.abort();
  signal?.addEventListener('abort', abort, {once: true});
  if (signal?.aborted) controller.abort();
  const timeout = setTimeout(abort, 15000);
  try {
    const response = await request(DISPATCH_URL, {
      method: 'POST', headers: {'Content-Type': 'application/json'},
      body: JSON.stringify(dispatchRequest(deviceId)), signal: controller.signal, redirect: 'error',
    });
    if (!response.ok) throw new Error(`Layanan cloud V380 tidak tersedia (HTTP ${response.status}).`);
    const result = await response.json();
    if (result.code !== 2000 || !Array.isArray(result.data)) throw new Error('Cloud V380 belum memberikan server relay untuk kamera ini.');
    const hosts = [...new Set(result.data.map(item => item?.ip).filter(ip => typeof ip === 'string' && net.isIP(ip)))].slice(0,4);
    for (const host of hosts) {
      if (controller.signal.aborted) break;
      if (await probe(host, 8800, controller.signal)) return {host, port:8800};
    }
    throw new Error('Server relay V380 tidak dapat dijangkau dari jaringan ini.');
  } catch (error) {
    if (controller.signal.aborted) throw new Error('Pencarian relay cloud dibatalkan atau melewati batas waktu.');
    throw error;
  } finally {clearTimeout(timeout);signal?.removeEventListener('abort', abort);}
}
module.exports = { resolveRelay, dispatchRequest };
