'use strict';
// Deterministic V380 replay: 3 H264 cameras + 1 JPEG camera, 16 fps, AES media encryption.
const net=require('node:net'),fs=require('node:fs'),crypto=require('node:crypto');
const dir=process.env.FIXTURE_DIR || '/media';
const raw=fs.readFileSync(`${dir}/camera.h264`),jpeg=fs.readFileSync(`${dir}/camera.jpg`);
const starts=[];for(let i=0;i<raw.length-5;i++){
 if(raw[i]===0&&raw[i+1]===0&&((raw[i+2]===1&&raw[i+3]===9)||(raw[i+2]===0&&raw[i+3]===1&&raw[i+4]===9))) {
  starts.push(i);if(raw[i+2]===0)i++;
 }
}
const frames=starts.map((start,i)=>raw.subarray(start,starts[i+1]||raw.length));
if(!frames.length)throw Error('No H264 AUD frames');
const key=Buffer.alloc(16);key.writeUInt32LE(48026);key.writeBigUInt64LE(0x618123462c14795cn,4);key.writeUInt32LE(0x82800df0,12);
function packet(payload,type){
 payload=Buffer.from(payload);
 for(let i=0;i+64<=payload.length;i+=80){const c=crypto.createCipheriv('aes-128-ecb',key,null);c.setAutoPadding(false);Buffer.concat([c.update(payload.subarray(i,i+64)),c.final()]).copy(payload,i);}
 const inner=Buffer.alloc(16);inner.writeUInt16LE(type,4);inner.writeUInt16LE(16,6);
 const data=Buffer.concat([inner,payload]),count=Math.ceil(data.length/500),parts=[];
 for(let n=0;n<count;n++){const piece=data.subarray(n*500,(n+1)*500),h=Buffer.alloc(12);h[0]=127;h[1]=type;h.writeUInt16LE(count,3);h.writeUInt16LE(n,5);h.writeUInt16LE(piece.length,7);parts.push(h,piece);}
 return Buffer.concat(parts);
}
const h264=frames.map(frame=>packet(frame,/000001(?:25|65)/.test(frame.toString('hex'))?0:1));
const jpg=packet(jpeg,9);
net.createServer(socket=>{
 let input=Buffer.alloc(0),timer,id=0,index=0,streaming=false;
 socket.setNoDelay(true);socket.on('error',()=>{});socket.once('close',()=>clearInterval(timer));
 socket.on('data',chunk=>{
  if(streaming)return;
  input=Buffer.concat([input,chunk]);
  while(input.length>=256){
   const cmd=input.subarray(0,256);input=input.subarray(256);const type=cmd.readUInt32LE();
   if(type===1167){const r=Buffer.alloc(256);r.writeUInt32LE(1168);r.writeUInt32LE(1001,4);r[12]=31;r.writeUInt32LE(48026,13);r.writeUInt32LE(100,17);socket.end(r);return;}
   if(type===301){id=cmd.readUInt32LE(4);const r=Buffer.alloc(32);r.writeUInt32LE(401);r.writeUInt32LE(1001,4);r.writeUInt16LE(20,8);r.writeUInt32LE(id===90000004?640:1280,10);r.writeUInt32LE(id===90000004?480:720,14);socket.write(r);}
   if(type===303){streaming=true;const send=()=>{if(socket.writableLength>4*1024*1024)return socket.destroy();socket.write(id===90000004?jpg:h264[index++%h264.length]);};send();timer=setInterval(send,1000/16);return;}
  }
 });
}).listen(8800,'0.0.0.0',()=>console.log(`Replay ready: ${frames.length} H264 frames, JPEG ${jpeg.length} bytes, 16 fps, AES-128`));
