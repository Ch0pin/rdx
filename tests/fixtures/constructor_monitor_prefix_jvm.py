from pathlib import Path
import hashlib, json, os, subprocess, sys
base = Path(sys.argv[1])
jdk = Path(os.environ['RDX_JAVA25_HOME']) / 'bin'
folder = base / 'sample'
folder.mkdir(exist_ok=True)
fields = '''
 public static Object Lock = new Object(); public static int counter;
 public final int id; public final String name;
 public final float f1, f2, f3, f4; public final Object peer;
 public final long wide; public final int number; public final boolean flag;
'''
hashes = {}
for name, file in [('Prefix', 'normal.java.txt'), ('FaultPrefix', 'fault.java.txt'), ('CapturedPrefix', 'captured.java.txt')]:
    source = (base / file).read_text()
    hashes[name] = hashlib.sha256(source.encode()).hexdigest()
    (folder / (name + '.java')).write_text('package sample; public class ' + name + ' {\n' + fields + source + '\n}\n')
(base / 'Oracle.java').write_text('''
import java.util.concurrent.*;
public class Oracle {
 static void verify(boolean result) { if (!result) throw new AssertionError(); }
 public static void main(String[] ignored) throws Exception {
  int count=0;
  float[] floats={0.0f,-0.0f,Float.NaN,Float.intBitsToFloat(0x7fc00001),Float.NEGATIVE_INFINITY,Float.POSITIVE_INFINITY,Float.MIN_VALUE};
  long[] longs={0,1,-1,Long.MIN_VALUE,Long.MAX_VALUE,4294967297L};
  int[] ints={0,1,-1,Integer.MIN_VALUE,Integer.MAX_VALUE};
  for(int initial:new int[]{0,Integer.MAX_VALUE}) {
   sample.Prefix.counter=initial;sample.CapturedPrefix.counter=initial;int expected=initial;
   for(int seed=0;seed<512;seed++) {
    String name=seed%3==0?null:new String("value"); Object peer=seed%2==0?null:new Object();
    float a=floats[seed%floats.length],b=floats[(seed+1)%floats.length],c=floats[(seed+2)%floats.length],d=floats[(seed+3)%floats.length];
    long wide=longs[seed%longs.length];int number=ints[seed%ints.length];boolean flag=seed%2==0;
    sample.Prefix actual=new sample.Prefix(name,a,b,c,d,peer,wide,number,flag);
    sample.CapturedPrefix captured=new sample.CapturedPrefix(name,a,b,c,d,peer,wide,number,flag);
    verify(captured.id==expected);expected++;
    verify(sample.CapturedPrefix.counter==expected&&captured.name==name&&captured.peer==peer&&captured.wide==wide&&captured.number==number&&captured.flag==flag);
    verify(Float.floatToRawIntBits(captured.f1)==Float.floatToRawIntBits(a)&&Float.floatToRawIntBits(captured.f2)==Float.floatToRawIntBits(b)&&Float.floatToRawIntBits(captured.f3)==Float.floatToRawIntBits(c)&&Float.floatToRawIntBits(captured.f4)==Float.floatToRawIntBits(d));count++;
    verify(actual.id==expected&&sample.Prefix.counter==expected);
    verify(actual.name==name&&actual.peer==peer&&actual.wide==wide&&actual.number==number&&actual.flag==flag);
    verify(Float.floatToRawIntBits(actual.f1)==Float.floatToRawIntBits(a));
    verify(Float.floatToRawIntBits(actual.f2)==Float.floatToRawIntBits(b));
    verify(Float.floatToRawIntBits(actual.f3)==Float.floatToRawIntBits(c));
    verify(Float.floatToRawIntBits(actual.f4)==Float.floatToRawIntBits(d));count++;
   }
  }
  // Contending constructors must retain their own captured counter value.
  // A read accidentally moved past monitor-exit can pass all serial cases.
  sample.Prefix.counter=0;sample.CapturedPrefix.counter=0;
  ConcurrentSkipListSet<Integer> ids=new ConcurrentSkipListSet<>();
  ConcurrentSkipListSet<Integer> capturedIds=new ConcurrentSkipListSet<>();
  CountDownLatch start=new CountDownLatch(1);
  java.util.concurrent.atomic.AtomicReference<Throwable> workerFailure=new java.util.concurrent.atomic.AtomicReference<>();
  Thread[] workers=new Thread[4];
  for(int worker=0;worker<workers.length;worker++) {
   workers[worker]=new Thread(()->{
    try { start.await(); for(int i=0;i<64;i++) {
     sample.Prefix instance=new sample.Prefix(null,0,0,0,0,null,0,0,false);
     verify(ids.add(instance.id));
     sample.CapturedPrefix captured=new sample.CapturedPrefix(null,0,0,0,0,null,0,0,false);verify(capturedIds.add(captured.id));
    }} catch(Throwable error) {workerFailure.compareAndSet(null,error);}
   });
   workers[worker].setDaemon(true);workers[worker].start();
  }
  start.countDown();
  for(Thread worker:workers) {worker.join(5000);verify(!worker.isAlive());}
  verify(workerFailure.get()==null&&sample.Prefix.counter==256&&ids.size()==256&&ids.first()==1&&ids.last()==256);
  verify(sample.CapturedPrefix.counter==256&&capturedIds.size()==256&&capturedIds.first()==0&&capturedIds.last()==255);
  sample.FaultPrefix.counter=10; boolean failed=false;
  try {new sample.FaultPrefix(null,0,0,0,0,null,0,0,false);} catch(ArithmeticException exception) {failed=true;}
  verify(failed&&sample.FaultPrefix.counter==10);
  CountDownLatch released=new CountDownLatch(1);
  Thread other=new Thread(()->{synchronized(sample.FaultPrefix.Lock){released.countDown();}});
  other.setDaemon(true);other.start();verify(released.await(2,TimeUnit.SECONDS));other.join(2000);
  sample.FaultPrefix success=new sample.FaultPrefix(null,0,0,0,0,null,0,1,false);
  verify(success.id==10&&sample.FaultPrefix.counter==10);
  System.out.println("PASS "+count+" exact-source constructor state comparisons; 512 contending constructions; fault type/counter/monitor release and later success");
 }
}
''')
command=[str(jdk/'javac'),'--release','25','-Xlint:all','-Werror','-d',str(base),*[str(p) for p in folder.glob('*.java')],str(base/'Oracle.java')]
result=subprocess.run(command,capture_output=True,text=True,timeout=60)
(base/'javac.log').write_text(result.stdout+result.stderr)
assert result.returncode==0,result.stderr
for name in ['sample.Prefix','sample.FaultPrefix']:
    result=subprocess.run([str(jdk/'javap'),'-p','-c','-classpath',str(base),name],capture_output=True,text=True,check=True,timeout=60)
    (base/(name+'.javap.txt')).write_text(result.stdout)
result=subprocess.run([str(jdk/'java'),'-cp',str(base),'Oracle'],capture_output=True,text=True,timeout=60)
(base/'jvm.log').write_text(result.stdout+result.stderr)
assert result.returncode==0,result.stderr
proof={'result':result.stdout.strip(),'unchanged_source_sha256':hashes,'model':'Structural fixture fields and own static lock/counter; exact emitted constructor bodies unchanged; division fault fixture checks exception class, counter state and cross-thread monitor release'}
(base/'jvm-evidence.json').write_text(json.dumps(proof,indent=2)+'\n')
print(json.dumps(proof,indent=2))
