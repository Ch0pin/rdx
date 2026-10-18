# Root executes this bounded unchanged-source oracle serially.
import hashlib, json, os, pathlib, shutil, subprocess, sys
base=pathlib.Path(sys.argv[1]).resolve()
jdk=pathlib.Path(os.environ['RDX_JAVA25_HOME'])
sdk=pathlib.Path(os.environ['RDX_ANDROID35_JAR'])
assert hashlib.sha256(sdk.read_bytes()).hexdigest()=='4566663c3876e022b4fa4ced8c8697c4ab1688267f090114fd92d027b32e619b'
sources=base/'sources'; sources.mkdir(exist_ok=True)
def write(name,text):
    p=sources/name;p.parent.mkdir(parents=True,exist_ok=True);p.write_text(text)
def run(argv):
    result=subprocess.run([str(x) for x in argv],capture_output=True,text=True,timeout=60)
    assert len(result.stdout)+len(result.stderr)<1024*1024
    assert result.returncode==0,(argv,result.stdout,result.stderr)
    return result.stdout
hashes={}
for name in ['Bridge.java','Child.java','Factory.java']:
    content=(base/name).read_bytes();hashes[name]=hashlib.sha256(content).hexdigest()
    (sources/'sample').mkdir(exist_ok=True);(sources/'sample'/name).write_bytes(content)
for name in ['create.fragment','copy.fragment','effectful.fragment']:
    content=(base/name).read_bytes();hashes[name]=hashlib.sha256(content).hexdigest()
write('sample/Hooks.java','package sample; public class Hooks {public static void bridge(){} public static void child(){} public static void before(){} public static void after(Child value){}}')
api=base/'api35';api.mkdir(exist_ok=True)
run([jdk/'bin/javac','--release','25','-Xlint:all','-Werror','-cp',sdk,'-d',api,*sorted(sources.rglob('*.java'))])
write('android/view/ViewGroup.java', '''package android.view;
public class ViewGroup {
 public static class LayoutParams {
  public int width,height;
  public LayoutParams(int width,int height){this.width=width;this.height=height;}
  public LayoutParams(LayoutParams input){if(input==null)throw sample.Hooks.NULL;this.width=input.width;this.height=input.height;}
 }
 public static class MarginLayoutParams extends LayoutParams {
  public int leftMargin;
  public MarginLayoutParams(int width,int height){super(width,height);}
  public MarginLayoutParams(LayoutParams input){super(input);}
 }
}''')
write('android/widget/LinearLayout.java', '''package android.widget;
public class LinearLayout {
 // Intentional oracle observation of fresh defaults and allocation identity.
 @SuppressWarnings("this-escape")
 public static class LayoutParams extends android.view.ViewGroup.MarginLayoutParams {
  static {sample.Hooks.sdk();}
  public Object input;
  public LayoutParams(int width,int height){super(width,height);sample.Hooks.parent(this,"INTS",null);if(width==99)throw sample.Hooks.FAIL;}
  public LayoutParams(android.view.ViewGroup.LayoutParams input){super(input);this.input=input;sample.Hooks.parent(this,"BASE",input);if(input.width==99)throw sample.Hooks.FAIL;}
  public LayoutParams(android.view.ViewGroup.MarginLayoutParams input){super(input);this.input=input;sample.Hooks.parent(this,"MARGIN",input);if(input.width==99)throw sample.Hooks.FAIL;}
 }
}''')
write('sample/Hooks.java', '''package sample;
public class Hooks {
 public static final RuntimeException FAIL=new IllegalStateException("constructor");
 public static final RuntimeException BEFORE=new IllegalStateException("before");
 public static final NullPointerException NULL=new NullPointerException("input");
 public static final RuntimeException INIT=new IllegalStateException("initialization");
 public static final java.util.List<String> events=new java.util.ArrayList<>();
 public static int mode;
 public static Object captured,input;
 public static void sdk(){events.add("sdk");if(mode==1)throw INIT;}
 public static void bridge(){events.add("bridge");if(mode==2)throw INIT;}
 public static void child(){events.add("child");if(mode==3)throw INIT;}
 public static void before(){events.add("before");if(mode==4)throw BEFORE;}
 public static void parent(Object value,String overload,Object arg){captured=value;input=arg;Child c=(Child)value;if(c.flag||c.value!=0)throw new AssertionError("fresh defaults");events.add(overload);}
 public static void after(Child value){if(value!=captured)throw new AssertionError("identity");events.add("after");}
}''')
write('Oracle.java', '''import java.lang.reflect.*;
import java.net.*;
import java.nio.file.*;
import java.util.*;
public class Oracle {
 public static void main(String[] args)throws Exception {
  int[] widths={-1,0,1,99,Integer.MIN_VALUE,Integer.MAX_VALUE};int[] heights={-2,0,1,Integer.MAX_VALUE};
  for(int seed=0;seed<256;seed++) {
   try(URLClassLoader loader=new URLClassLoader(new URL[]{Path.of(args[0]).toUri().toURL()},ClassLoader.getPlatformClassLoader())) {
    Class<?> hooks=loader.loadClass("sample.Hooks");Class.forName(hooks.getName(),true,loader);
    int mode=(seed/2)%16;mode=mode<=3?mode:0;hooks.getField("mode").setInt(null,mode);
    Class<?> factory=loader.loadClass("sample.Factory"),base=loader.loadClass("android.view.ViewGroup$LayoutParams");
    boolean copy=(seed&1)!=0,isNull=seed==255;int width=widths[(seed/2)%widths.length],height=heights[(seed/12)%heights.length];
    Object input=null;if(copy&&!isNull){Class<?> type=(seed&2)!=0?loader.loadClass("android.view.ViewGroup$MarginLayoutParams"):base;input=type.getConstructor(int.class,int.class).newInstance(width,height);}
    Object returned=null;Throwable fault=null;
    try{returned=copy?factory.getMethod("copy",base).invoke(null,input):factory.getMethod("create",int.class,int.class).invoke(null,width,height);}catch(InvocationTargetException error){fault=error.getCause();}
    List<String> expected=new ArrayList<>();expected.add("before");expected.add("sdk");
    if(mode!=1){expected.add("bridge");if(mode!=2)expected.add("child");}
    if(mode==1||mode==2||mode==3){if(!(fault instanceof ExceptionInInitializerError)||fault.getCause()!=hooks.getField("INIT").get(null))throw new AssertionError("clinit fault "+seed);}
    else {
     if(!isNull)expected.add(copy?"BASE":"INTS");
     if(isNull||width==99){Object wanted=hooks.getField(isNull?"NULL":"FAIL").get(null);if(fault!=wanted)throw new AssertionError("fault identity "+seed);}
     else {expected.add("after");if(fault!=null||returned!=hooks.getField("captured").get(null))throw new AssertionError("result "+seed);
      if(returned.getClass().getField("flag").getBoolean(returned)||returned.getClass().getField("value").getInt(returned)!=0)throw new AssertionError("defaults");
      if(base.getField("width").getInt(returned)!=width||base.getField("height").getInt(returned)!=height)throw new AssertionError("parent values");
      if(copy&&hooks.getField("input").get(null)!=input)throw new AssertionError("argument identity");
     }
     if(copy&&!isNull&&(base.getField("width").getInt(input)!=width||base.getField("height").getInt(input)!=height))throw new AssertionError("input mutation");
    }
    if(!expected.equals(hooks.getField("events").get(null)))throw new AssertionError(seed+" "+expected+" "+hooks.getField("events").get(null));
   }
  }
  int effectConstructorFaults=0,effectInitFailures=0;
  for(int seed=0;seed<256;seed++) {
   try(URLClassLoader loader=new URLClassLoader(new URL[]{Path.of(args[0]).toUri().toURL()},ClassLoader.getPlatformClassLoader())) {
    Class<?> hooks=loader.loadClass("sample.Hooks");Class.forName(hooks.getName(),true,loader);
    int mode=seed%4;hooks.getField("mode").setInt(null,mode);
    Class<?> factory=loader.loadClass("sample.Factory");int width=widths[(seed/4)%widths.length],height=heights[(seed/24)%heights.length];
    Object returned=null;Throwable fault=null;
    try{returned=factory.getMethod("effectful",int.class,int.class).invoke(null,width,height);}catch(InvocationTargetException error){fault=error.getCause();}
    List<String> expected=new ArrayList<>();expected.add("sdk");
    if(mode!=1){expected.add("bridge");if(mode!=2){expected.add("child");if(mode!=3){expected.add("before");expected.add("INTS");}}}
    if(mode==1||mode==2||mode==3){effectInitFailures++;if(!(fault instanceof ExceptionInInitializerError)||fault.getCause()!=hooks.getField("INIT").get(null))throw new AssertionError("effect clinit fault "+seed);}
    else if(width==99){effectConstructorFaults++;if(fault!=hooks.getField("FAIL").get(null))throw new AssertionError("effect constructor fault "+seed);}
    else {if(fault!=null||returned!=hooks.getField("captured").get(null))throw new AssertionError("effect result "+seed);
     if(returned.getClass().getField("flag").getBoolean(returned)||returned.getClass().getField("value").getInt(returned)!=0)throw new AssertionError("effect defaults");
     if(returned.getClass().getField("width").getInt(returned)!=width||returned.getClass().getField("height").getInt(returned)!=height)throw new AssertionError("effect parent values");
    }
    if(!expected.equals(hooks.getField("events").get(null)))throw new AssertionError("effect "+seed+" "+expected+" "+hooks.getField("events").get(null));
   }
  }
  if(effectConstructorFaults!=11||effectInitFailures!=192)throw new AssertionError("dead effect coverage "+effectConstructorFaults+" "+effectInitFailures);
  int effectBeforeFaults=0;
  for(int seed=0;seed<64;seed++) {
   try(URLClassLoader loader=new URLClassLoader(new URL[]{Path.of(args[0]).toUri().toURL()},ClassLoader.getPlatformClassLoader())) {
    Class<?> hooks=loader.loadClass("sample.Hooks");Class.forName(hooks.getName(),true,loader);hooks.getField("mode").setInt(null,4);
    Class<?> factory=loader.loadClass("sample.Factory");int width=widths[seed%widths.length],height=heights[(seed/widths.length)%heights.length];Throwable fault=null;
    try{factory.getMethod("effectful",int.class,int.class).invoke(null,width,height);}catch(InvocationTargetException error){fault=error.getCause();}
    if(fault!=hooks.getField("BEFORE").get(null))throw new AssertionError("before fault identity "+seed);
    if(hooks.getField("captured").get(null)!=null)throw new AssertionError("parent ran after before fault "+seed);
    List<String> expected=List.of("sdk","bridge","child","before");
    if(!expected.equals(hooks.getField("events").get(null)))throw new AssertionError("before "+seed+" "+expected+" "+hooks.getField("events").get(null));
    effectBeforeFaults++;
   }
  }
  if(effectBeforeFaults!=64)throw new AssertionError("dead before-fault coverage "+effectBeforeFaults);
  System.out.println("576 unchanged-source overload/default/fault/clinit/effect cases passed; effect constructor faults=11 init failures=192 before faults=64");
 }
}''')
classes=base/'model';classes.mkdir(exist_ok=True)
run([jdk/'bin/javac','--release','25','-Xlint:all','-Werror','-d',classes,*sorted(sources.rglob('*.java'))])
output=run([jdk/'bin/java','-cp',classes,'Oracle',classes])
bytecode=run([jdk/'bin/javap','-p','-c','-classpath',classes,'sample.Bridge','sample.Child','sample.Factory'])
(base/'javap.txt').write_text(bytecode)
evidence={'source_sha256':hashes,'api35_compile':'unchanged whole-class generated sources','raw_method_fragments':'diagnostic evidence only; binary nested names require whole-class readable normalization','cases':576,'effect_constructor_fault_cases':11,'effect_init_failure_cases':192,'effect_before_fault_cases':64,'result':output.strip(),'scope':'bounded SDK models for forwarding/caller semantics; exact API35 signatures compile separately','loaders':'576 resource-owning loaders closed deterministically'}
(base/'evidence.json').write_text(json.dumps(evidence,indent=2)+'\n');print(json.dumps(evidence))
