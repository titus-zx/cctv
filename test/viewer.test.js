'use strict';
const {test}=require('node:test');
const assert=require('node:assert/strict');
const {Viewer}=require('../lib/viewer');

test('cloud resolution is read from JPEG SOF after application metadata',()=>{
  const viewer=new Viewer({mode:'cloud'});
  const jpeg=Buffer.from('ffd8ffe000040000ffc000110802d0050003011100021100031100ffd9','hex');
  viewer.publish(jpeg);
  assert.equal(viewer.info.width,1280);assert.equal(viewer.info.height,720);
  assert.equal(viewer.state,'live');assert.equal(viewer.lastJpeg,jpeg);
  viewer.stop();
});
test('truncated JPEG metadata does not cause a buffer read exception',()=>{
  const viewer=new Viewer({mode:'cloud'});
  viewer.publish(Buffer.from('ffd8ffc0001108','hex'));
  assert.equal(viewer.info.width,undefined);assert.equal(viewer.info.height,undefined);
  viewer.stop();
});
