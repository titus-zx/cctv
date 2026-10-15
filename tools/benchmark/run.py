#!/usr/bin/env python3
"""Compare container CPU/memory and HTTP performance with equal MJPEG viewing load."""
import argparse,concurrent.futures,http.client,json,os,pathlib,socket,statistics,subprocess,threading,time,urllib.request
ROOT=pathlib.Path(__file__).resolve().parents[2]
VERSIONS={'node':3381,'rust':3382}
class Docker(http.client.HTTPConnection):
 def __init__(self):super().__init__('localhost');self.timeout=10
 def connect(self):
  self.sock=socket.socket(socket.AF_UNIX,socket.SOCK_STREAM);self.sock.settimeout(self.timeout)
  self.sock.connect(os.environ.get('DOCKER_HOST','unix://'+str(pathlib.Path.home()/'.docker/run/docker.sock')).removeprefix('unix://'))
def docker(path):
 c=Docker();c.request('GET',path);r=c.getresponse();data=json.loads(r.read());c.close()
 if r.status!=200:raise RuntimeError(data)
 return data
def api(version):return json.load(urllib.request.urlopen(f'http://localhost:{VERSIONS[version]}/api/cameras',timeout=8))
def stats(version):
 s=docker(f'/containers/v380-compare-{version}-1/stats?stream=false&one-shot=true');m=s['memory_stats'];d=m.get('stats',{})
 return {'cpu_ns':s['cpu_stats']['cpu_usage']['total_usage'],'memory_mib':(m.get('usage',0)-d.get('inactive_file',d.get('total_inactive_file',0)))/1048576,'pids':s.get('pids_stats',{}).get('current',0),'time':time.perf_counter()}
class Stream:
 def __init__(self,version,camera):
  self.version=version;self.camera=camera;self.frames=0;self.bytes=0;self.first_ms=None;self.error=None;self.stop=threading.Event();self.response=None
  self.thread=threading.Thread(target=self.run,daemon=True);self.thread.start()
 def run(self):
  start=time.perf_counter();previous=b''
  try:
   with urllib.request.urlopen(f'http://localhost:{VERSIONS[self.version]}/stream/{self.camera}',timeout=20) as r:
    self.response=r
    while not self.stop.is_set():
     chunk=r.read1(65536)
     if not chunk:break
     count=(previous+chunk).count(b'\xff\xd9');previous=chunk[-1:]
     self.frames+=count;self.bytes+=len(chunk)
     if count and self.first_ms is None:self.first_ms=(time.perf_counter()-start)*1000
  except Exception as e:
   if not self.stop.is_set():self.error=str(e)
 def close(self):
  self.stop.set()
  if self.response:
   try:self.response.close()
   except OSError:pass
  self.thread.join(timeout=2)
def sample(seconds,streams):
 start={v:stats(v) for v in VERSIONS};frames={(s.version,s.camera):s.frames for s in streams};bytes_={(s.version,s.camera):s.bytes for s in streams}
 mem={v:[] for v in VERSIONS};end=time.perf_counter()+seconds
 while time.perf_counter()<end:
  time.sleep(min(2,max(0,end-time.perf_counter())))
  for v in VERSIONS:mem[v].append(stats(v)['memory_mib'])
 finish={v:stats(v) for v in VERSIONS};result={}
 for v in VERSIONS:
  duration=finish[v]['time']-start[v]['time'];vs=[s for s in streams if s.version==v]
  result[v]={'cpu_percent_one_core':(finish[v]['cpu_ns']-start[v]['cpu_ns'])/(duration*1e9)*100,
   'memory_mean_mib':statistics.mean(mem[v]),'memory_peak_mib':max(mem[v]),'duration_seconds':duration,
   'streams':[{'camera':s.camera,'fps':(s.frames-frames[(v,s.camera)])/duration,'mib_per_second':(s.bytes-bytes_[(v,s.camera)])/duration/1048576,'first_frame_ms':s.first_ms,'error':s.error} for s in vs]}
 return result
def latency(version,count=300,rate=100):
 connection=http.client.HTTPConnection('localhost',VERSIONS[version],timeout=5);times=[];errors=0;start=time.perf_counter()
 for n in range(count):
  target=start+n/rate;time.sleep(max(0,target-time.perf_counter()));begin=time.perf_counter()
  try:
   connection.request('GET','/api/cameras');r=connection.getresponse();r.read()
   if r.status==200:times.append((time.perf_counter()-begin)*1000)
   else:errors+=1
  except Exception:
   errors+=1;connection.close();connection=http.client.HTTPConnection('localhost',VERSIONS[version],timeout=5)
 connection.close();times.sort()
 return {'requests':count,'errors':errors,'target_rps':rate,'p50_ms':statistics.median(times) if times else None,'p95_ms':times[min(len(times)-1,int(len(times)*.95))] if times else None}
def main():
 p=argparse.ArgumentParser();p.add_argument('--label',required=True);p.add_argument('--seconds',type=int,default=20);p.add_argument('--rounds',type=int,default=3);args=p.parse_args()
 output={'label':args.label,'arch':docker('/info').get('Architecture'),'cpus_per_container':2,'memory_limit_mib':1024,'preview_fps':5,'recording':False,'results':{}}
 print('Waiting for camera sources',flush=True)
 for _ in range(60):
  states={v:api(v) for v in VERSIONS}
  if all(all(c['state']=='live' for c in cams) for cams in states.values()):break
  time.sleep(1)
 output['initial_cameras']=states
 common=[i for i in range(4) if all(states[v][i]['state']=='live' for v in VERSIONS)]
 output['camera_ids_tested']=common
 if not common:raise RuntimeError('No common live camera sources')
 print('Matched cameras:',common,flush=True)
 # Allow demand-driven preview decoders from earlier HTTP clients to stop.
 time.sleep(8)
 output['idle']=sample(args.seconds,[]);print('Idle complete',flush=True)
 streams=[Stream(v,i) for v in VERSIONS for i in common]
 try:
  deadline=time.perf_counter()+25
  while time.perf_counter()<deadline and not all(s.first_ms is not None for s in streams):time.sleep(.25)
  if not all(s.first_ms is not None for s in streams):raise RuntimeError('Missing preview: '+str([(s.version,s.camera,s.error) for s in streams if s.first_ms is None]))
  time.sleep(8)
  output['streaming_rounds']=[]
  for n in range(args.rounds):
   output['streaming_rounds'].append(sample(args.seconds,streams));print('Streaming round',n+1,'complete',flush=True)
  output['api_latency']={v:[] for v in VERSIONS}
  for order in [('node','rust'),('rust','node')]:
   for v in order:output['api_latency'][v].append(latency(v))
  output['final_cameras']={v:api(v) for v in VERSIONS}
 finally:
  for s in streams:s.close()
 for v in VERSIONS:
  rounds=[r[v] for r in output['streaming_rounds']]
  output['results'][v]={'cpu_percent':statistics.mean(r['cpu_percent_one_core'] for r in rounds),'memory_mib':statistics.mean(r['memory_mean_mib'] for r in rounds),
   'fps_total':statistics.mean(sum(s['fps'] for s in r['streams']) for r in rounds),
   'api_p50_ms':statistics.mean(r['p50_ms'] for r in output['api_latency'][v]),'api_p95_ms':statistics.mean(r['p95_ms'] for r in output['api_latency'][v]),
   'api_errors':sum(r['errors'] for r in output['api_latency'][v])}
 path=ROOT/'.benchmark/results'/f'{args.label}.json';path.write_text(json.dumps(output,indent=2)+'\n');os.chmod(path,0o600)
 print(json.dumps(output['results'],indent=2),flush=True)
if __name__=='__main__':main()
