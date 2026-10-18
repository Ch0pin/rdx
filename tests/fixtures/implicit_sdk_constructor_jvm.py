from pathlib import Path
import json, subprocess, re, hashlib, os, sys
base=Path(sys.argv[1])
jdk=Path(os.environ['RDX_JAVA25_HOME'])/'bin'
items=[json.loads(l) for l in (base/'exact.jsonl').read_text().splitlines()]
out=base/'jvm';out.mkdir(exist_ok=True)
def java_type(d):
    if d=='I':return 'int'
    if d.startswith('['):return java_type(d[1:])+'[]'
    return d[1:-1].replace('/','.')
for pkg,ancestors in [('hh',[('a','java.util.AbstractCollection<Object>'),('d','a'),('f','a')]),('of',[('a','java.util.AbstractCollection<Object>'),('e','a')])]:
    folder=out/pkg;folder.mkdir(exist_ok=True)
    for name,parent in ancestors:
        fields = 'public transient hh.d b;' if pkg == 'hh' and name == 'f' else ''
        (folder/f'{name}.java').write_text(f'package {pkg}; public abstract class {name} extends {parent} {{{fields}}}\n')
(out/'hh/l.java').write_text('package hh; public class l extends d {public int size(){return 0;} public java.util.Iterator<Object> iterator(){return java.util.Collections.emptyIterator();}}\n')
for x in items:
    pkg,name=x['name'].split('.')
    fields='\n'.join('public '+('final ' if f['flags']&16 else '')+('transient ' if f['flags']&128 else '')+java_type(f['type'])+' '+f['name']+';' for f in x['declarations'])
    source=f'package {pkg}; public class {name} extends {java_type(x["parent"])} {{\n{fields}\n{x["source"]}\npublic int size(){{return 0;}} public java.util.Iterator<Object> iterator(){{return java.util.Collections.emptyIterator();}}\n}}\n'
    (out/pkg/f'{name}.java').write_text(source)
rows=[]
for x in items:
    names=[];indices=[]
    mapping={f['index']:f['name'] for f in x['fields']}
    for idx,name in sorted(mapping.items()):indices.append(idx);names.append('"'+name+'"')
    rows.append('new Fixture("'+x['name']+'",'+str(x['registers'])+','+str(x['ins'])+',new int[]{'+','.join(map(str,x['words']))+'},new int[]{'+','.join(map(str,indices))+'},new String[]{'+','.join(names)+'})')
oracle=r'''import java.util.*; import java.lang.reflect.*;
public class Oracle {
 record Fixture(String name,int registers,int ins,int[] words,int[] indices,String[] fields) {}
 static Object peer(Class<?> t) {
  if(t==hh.d.class)return new hh.g(0,null);
  if(t==hh.i.class)return new hh.i(null,null,0);
  if(t==hh.l.class)return new hh.l();
  if(t==hh.k.class)return new hh.k(0,0,null);
  if(t==of.e.class)return new of.f(0,null);
  throw new AssertionError(t);
 }
 static Object argument(Class<?> t,int seed,int index){
  if(t==int.class){int[] ints={0,1,-1,Integer.MIN_VALUE,Integer.MAX_VALUE,37,-41};return ints[Math.floorMod(seed+index,ints.length)];}
  int choice=Math.floorMod(seed/(index+1),3);if(choice==0)return null;
  if(t==Object[].class)return choice==1?new Object[0]:new Object[]{new Object(),null,new Object()};
  return peer(t);
 }
 static int check(Fixture f,int seed)throws Exception {
  Class<?> c=Class.forName(f.name);Constructor<?> ctor=c.getDeclaredConstructors()[0]; Class<?>[] types=ctor.getParameterTypes();Object[] args=new Object[types.length];for(int i=0;i<args.length;i++)args[i]=argument(types[i],seed,i);
  Object[] regs=new Object[f.registers];Object marker=new Object();regs[f.registers-f.ins]=marker;for(int i=0;i<args.length;i++)regs[f.registers-f.ins+1+i]=args[i];Map<String,Object> expected=new HashMap<>();Map<Integer,String> fields=new HashMap<>();for(int i=0;i<f.indices.length;i++)fields.put(f.indices[i],f.fields[i]);
  int calls=0;for(int pc=0;pc<f.words.length;){int word=f.words[pc];switch(word&255){case 0x59,0x5b->{int value=(word>>8)&15,receiver=(word>>12)&15;if(regs[receiver]!=marker)throw new AssertionError("receiver");String field=fields.get(f.words[pc+1]);if(field==null)throw new AssertionError("field");expected.put(field,regs[value]);pc+=2;}case 0x70->{if((word>>12)!=1||(f.words[pc+2]&15)!=f.registers-f.ins)throw new AssertionError("super words");calls++;pc+=3;}case 0x0e->{if(pc+1!=f.words.length)throw new AssertionError("return");pc++;}default->throw new AssertionError("opcode "+(word&255));}}
  if(calls!=1)throw new AssertionError("calls");Object actual=ctor.newInstance(args);for(var entry:expected.entrySet()){Object value=c.getField(entry.getKey()).get(actual);if(value instanceof Integer){if(!value.equals(entry.getValue()))throw new AssertionError(f.name+" "+entry);}else if(value!=entry.getValue())throw new AssertionError(f.name+" identity "+entry);}
  if(!(actual instanceof java.util.AbstractCollection))throw new AssertionError("ancestry");
  if(actual instanceof hh.f && ((hh.f)actual).b!=null)throw new AssertionError("inherited default field"); return 1;
 }
 public static void main(String[] a)throws Exception {Fixture[] fixtures={FIXTURES};int count=0;for(Fixture f:fixtures)for(int seed=0;seed<1536;seed++)count+=check(f,seed);System.out.println("PASS "+count+" exact raw DEX/JVM constructor comparisons");}
}'''.replace('FIXTURES',',\n'.join(rows))
(out/'Oracle.java').write_text(oracle)
command=[str(jdk/'javac'),'-d',str(out),*[str(p) for p in out.rglob('*.java')]]
result=subprocess.run(command,capture_output=True,text=True);(out/'javac.log').write_text(result.stdout+result.stderr);assert result.returncode==0,result.stderr
order_evidence=[]
empty_parent_evidence=[]
for name in ['hh.a','hh.d','hh.f','of.a','of.e','java.util.AbstractCollection']:
    result=subprocess.run([str(jdk/'javap'),'-c','-p','-classpath',str(out),name],capture_output=True,text=True,check=True)
    match=re.search(r'(?:public|protected) '+re.escape(name)+r'\(\);\s+Code:\s*((?:\s*\d+: [^\n]*\n)+)',result.stdout)
    assert match, (name,result.stdout)
    body=match.group(1)
    assert [re.sub(r'^\s*\d+:\s*','',l).split()[0] for l in body.strip().splitlines()]==['aload_0','invokespecial','return'],(name,body)
    (out/(name+'.constructor.javap.txt')).write_text(body)
    empty_parent_evidence.append({'name':name,'opcodes':['aload_0','invokespecial','return']})
for x in items:
    result=subprocess.run([str(jdk/'javap'),'-c','-p','-classpath',str(out),x['name']],capture_output=True,text=True,check=True)
    (out/(x['name']+'.javap.txt')).write_text(result.stdout)
    constructor=result.stdout.split('public '+x['name']+'(')[1].split('public int size(')[0]
    actual=[]
    for line in constructor.splitlines():
        if 'putfield' in line:
            actual.append('store:'+re.search(r'// Field (\w+):',line).group(1))
        if 'invokespecial' in line:actual.append('super')
    expected=[];pc=0;mapping={f['index']:f['name'] for f in x['fields']}
    while pc<len(x['words']):
        op=x['words'][pc]&255
        if op in (0x59,0x5b):expected.append('store:'+mapping[x['words'][pc+1]]);pc+=2
        elif op==0x70:expected.append('super');pc+=3
        elif op==0x0e:pc+=1
        else:raise AssertionError(op)
    assert expected==actual,(x['name'],expected,actual)
    order_evidence.append({'name':x['name'],'order':actual,'source_sha256':hashlib.sha256(x['source'].encode()).hexdigest()})
result=subprocess.run([str(jdk/'java'),'-cp',str(out),'Oracle'],capture_output=True,text=True);(out/'jvm.log').write_text(result.stdout+result.stderr);assert result.returncode==0,result.stderr
evidence={'jvm':result.stdout.strip(),'unchanged_emitted_constructors':len(items),'compiled_order':order_evidence,'empty_parents':empty_parent_evidence,'sdk_endpoint':'java.util.AbstractCollection.<init>()V','parent_model':'Two empty implicit Java layers with inherited default fields, real JDK AbstractCollection implementation'}
(base/'exact-evidence.json').write_text(json.dumps(evidence,indent=2)+'\n');print(json.dumps(evidence,indent=2))
