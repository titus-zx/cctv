'use strict';
const { test } = require('node:test');
const assert = require('node:assert/strict');
const net = require('node:net');
const crypto = require('node:crypto');
const { once } = require('node:events');
const { V380Client, mediaKey, decodeVideo } = require('../lib/v380');

test('decrypts an actual camera capture: ticket 48026, H264 SPS/PPS and untouched 16-byte gap', () => {
  const encrypted = Buffer.from('01ffffff0000100051cd0237a1010000d368b7bae8fd08fa0c881f153c22f38fa032131d8e50a4f52f0adb14ad1a8085ea2a07aaf62ffb4c0866ac17151419b550f71394a2db5132c250f8fcb921b6708056bdc597ed85c658fd96b84d63f535', 'hex');
  const decoded = decodeVideo(encrypted, mediaKey(48026));
  assert.equal(decoded.subarray(0,64).toString('hex'), '00000001274de0288d680a02ff9610000003001000000301e0f1422a0000000128ee1c920000000125b8000415ff68b405b571cf3b968f3354e17f6a0ca2b424');
  assert.deepEqual(decoded.subarray(64,80),encrypted.subarray(80,96));
  assert.notDeepEqual(decodeVideo(encrypted,mediaKey(47962)).subarray(0,4), Buffer.from([0,0,0,1]));
});

async function mockCamera(t, loginResult=1001, password='test', frameType=0, cloud=false) {
  const sockets = new Set(); const errors = [];
  const server = net.createServer(socket => {
    sockets.add(socket);socket.once('close',()=>sockets.delete(socket));socket.on('error',()=>{});
    let input=Buffer.alloc(0);
    socket.on('data',chunk=>{
      input=Buffer.concat([input,chunk]);
      while(input.length>=4){
        const size=cloud && input.readUInt32LE(0)===1167 ? 520 : 256;
        if(input.length<size)break;
        const request=input.subarray(0,size);input=input.subarray(size);
        try {
          const cmd=request.readUInt32LE(0);
          if(cmd===1167){
            assert.equal(request.readUInt32LE(13),65986431);
            const usernameOffset=cloud ? 71 : 49, passwordOffset=cloud ? 103 : 81;
            assert.equal(request.subarray(usernameOffset,usernameOffset+5).toString(),'admin');
            if(cloud){
              assert.equal(request.readUInt32LE(4),1022);
              assert.equal(request.readUInt32LE(9),1);
              assert.equal(request.subarray(17,67).toString().split('\0')[0],'65986431.nvdvr.net');
              assert.equal(request.readUInt32LE(67),8800);
            }
            let plaintext = request.subarray(passwordOffset+16,passwordOffset+64);
            const staticKey = Buffer.alloc(16); staticKey.write('macrovideo+*#!^@');
            for (const key of [request.subarray(passwordOffset,passwordOffset+16),staticKey]) {
              const decipher = crypto.createDecipheriv('aes-128-ecb',key,null);
              decipher.setAutoPadding(false);
              plaintext = Buffer.concat([decipher.update(plaintext),decipher.final()]);
            }
            assert.equal(plaintext.subarray(0,Buffer.byteLength(password)).toString(),password);
            assert.ok(plaintext.subarray(Buffer.byteLength(password)).every(byte=>byte===0));
            const reply=Buffer.alloc(256);reply.writeUInt32LE(1168);reply.writeInt32LE(loginResult,4);reply[12]=30;reply.writeUInt32LE(48026,13);
            reply.writeUInt32LE(0x12345678,17);
            socket.write(reply.subarray(0,7));setImmediate(()=>socket.write(reply.subarray(7)));
          } else if(cmd===301){
            if(cloud){
              assert.equal(request.readUInt32LE(4),1022);
              assert.equal(request.subarray(8,58).toString().split('\0')[0],'65986431.nvdvr.net');
              assert.equal(request.readUInt32LE(58),8800);
              assert.equal(request.readUInt32LE(62),65986431);
              assert.equal(request.readUInt32LE(66),48026);
              assert.equal(request.readUInt32LE(70),0x12345678);
              assert.equal(request.readUInt32LE(74),0);
            } else assert.equal(request.readUInt32LE(14),48026);
            const reply=Buffer.alloc(412);reply.writeUInt32LE(401);reply.writeInt32LE(1001,4);reply.writeUInt16LE(20,8);reply.writeUInt32LE(1280,10);reply.writeUInt32LE(720,14);
            socket.write(reply);
          } else if(cmd===303){
            assert.equal(request.readUInt16LE(4),0x3001);
            const inner=Buffer.alloc(16);
            const video=Buffer.from(frameType===9 ? 'ffd8ffe000104a46494600010100000100010000ffd9' : '000000016742001f0000000168ee3c800000000165b801','hex');
            const full=Buffer.concat([inner,video]);const midpoint=19;
            for(const [index,payload] of [full.subarray(0,midpoint),full.subarray(midpoint)].entries()){
              const header=Buffer.alloc(12);header[0]=127;header[1]=frameType;header.writeUInt16LE(2,3);header.writeUInt16LE(index,5);header.writeUInt16LE(payload.length,7);
              const packet=Buffer.concat([header,payload]);socket.write(packet.subarray(0,3));socket.write(packet.subarray(3));
            }
          }
        }catch(error){errors.push(error);socket.destroy();}
      }
    });
  });
  server.listen(0,'127.0.0.1');await once(server,'listening');
  t.after(async()=>{for(const socket of sockets)socket.destroy();await new Promise(r=>server.close(r));assert.deepEqual(errors,[]);});
  return {ip:'127.0.0.1',v380Port:server.address().port,deviceId:'65986431',password,username:'admin'};
}
test('login, extended reply, fragmented TCP reads and complete frame assembly', {timeout:5000}, async t=>{
  const camera=await mockCamera(t);const client=new V380Client(camera);t.after(()=>client.stop());
  const frames=[];let failure;
  client.on('failure',error=>{failure=error;});
  client.on('frame',(data,info)=>{frames.push({data,info});client.stop();});
  await client.start();
  assert.equal(failure,undefined);assert.equal(frames.length,1);
  assert.equal(frames[0].data.toString('hex'),'000000016742001f0000000168ee3c800000000165b801');
  assert.deepEqual(frames[0].info,{keyframe:true,codec:'h264'});
});
test('wrong password produces explicit authentication failure and closes sockets', {timeout:5000}, async t=>{
  const camera=await mockCamera(t,1012);const client=new V380Client(camera);t.after(()=>client.stop());
  let failure;client.on('failure',error=>failure=error);await client.start();
  assert.equal(failure.code,'AUTH_1012');assert.equal(client.stopped,true);
});
test('stopping during connect closes the pending socket', {timeout:5000}, async t=>{
  const camera=await mockCamera(t);const client=new V380Client(camera);
  const pending=client.start();client.stop();await pending;assert.equal(client.stopped,true);
});
test('password spanning two AES blocks is sent intact in the 64-byte login field', {timeout:5000}, async t=>{
  const camera=await mockCamera(t,1001,'1234567890123456');
  const client=new V380Client(camera);t.after(()=>client.stop());
  let failure,frames=0;
  client.on('failure',error=>failure=error);
  client.on('frame',()=>{frames++;client.stop();});
  await client.start();
  assert.equal(failure,undefined);assert.equal(frames,1);
});
test('JPEG camera fragments emit an independent MJPEG frame without Annex-B validation', {timeout:5000}, async t=>{
  const camera=await mockCamera(t,1001,'test',9);
  const client=new V380Client(camera);t.after(()=>client.stop());
  let frame,failure;
  client.on('failure',error=>failure=error);
  client.on('frame',(data,info)=>{frame={data,info};client.stop();});
  await client.start();
  assert.equal(failure,undefined);
  assert.equal(frame.data.toString('hex'),'ffd8ffe000104a46494600010100000100010000ffd9');
  assert.deepEqual(frame.info,{keyframe:true,codec:'mjpeg'});
});
test('cloud login uses the resolved relay, cloud offsets and session ticket for streaming', {timeout:5000}, async t=>{
  const relay=await mockCamera(t,1001,'cloud-password',0,true);
  let calls=0;
  const client=new V380Client({...relay,ip:'192.0.2.1',mode:'cloud',quality:'sd'}, {
    resolveRelay:async(id,{signal})=>{
      assert.equal(id,relay.deviceId);assert.equal(signal.aborted,false);calls++;
      return {host:'127.0.0.1',port:relay.v380Port};
    },
  });
  t.after(()=>client.stop());let failure,frame;
  client.on('failure',error=>failure=error);
  client.on('frame',data=>{frame=data;client.stop();});
  await client.start();
  assert.equal(failure,undefined);assert.equal(calls,1);
  assert.equal(frame.toString('hex'),'000000016742001f0000000168ee3c800000000165b801');
});
test('stopping a cloud connection aborts relay lookup before authentication', async()=>{
  let lookupStarted;
  const ready=new Promise(resolve=>lookupStarted=resolve);
  const client=new V380Client({mode:'cloud',deviceId:'65986431'}, {
    resolveRelay:(_id,{signal})=>new Promise((_resolve,reject)=>{
      signal.addEventListener('abort',()=>reject(new Error('cancelled')),{once:true});lookupStarted();
    }),
  });
  let failure;client.on('failure',error=>failure=error);
  const task=client.start();await ready;client.stop();await task;
  assert.equal(failure,undefined);assert.equal(client.sockets.size,0);
});
test('ptz sends command 170 with idle 1000 and direction codes, and rejects bad input', () => {
  const {V380Client} = require('../lib/v380');
  const client = new V380Client({deviceId: '1', password: 'x'}); const sent = [];
  client.control = {destroyed: false, write: b => sent.push(b)};
  client.ptz('right'); client.ptz('up'); client.ptz('stop');
  assert.deepEqual(sent.map(b => [b.length, b.readUInt32LE(0), b.readUInt16LE(8), b.readUInt16LE(10)]),
    [[256,170,1002,1000],[256,170,1000,1003],[256,170,1000,1000]]);
  assert.throws(() => client.ptz('diagonal'), /Arah/);
  client.control = null; assert.throws(() => client.ptz('left'), /belum terhubung/);
});
