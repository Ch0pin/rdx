from pathlib import Path
import json, subprocess, os, sys, hashlib, re
base=Path(sys.argv[1]);data=json.loads((base/'exact.json').read_text());jdk=Path(os.environ['RDX_JAVA25_HOME'])/'bin';out=base/'jvm';out.mkdir(exist_ok=True)
def j(s):return json.dumps(s)
def ty(d):
    if d.startswith('['):return ty(d[1:])+'[]'
    return {'I':'int','J':'long','Z':'boolean','V':'void'}.get(d,d[1:-1].replace('/','.'))
rows=[];source_hashes=[];restored=[]
for c in data['classes']:
    pkg,name=c['name'].rsplit('.',1);folder=out/Path(pkg.replace('.','/'));folder.mkdir(parents=True,exist_ok=True)
    declarations=[]
    for f in c['declarations']:
        flags=f['flags'];access='public' if flags&1 else 'private' if flags&2 else 'protected' if flags&4 else ''
        initializer=' = new Object()' if f['static'] and f['name']=='NO_RECEIVER' else ''
        declarations.append(access+' '+('static ' if f['static'] else '')+('final ' if flags&16 else '')+ty(f['type'])+' '+f['name']+initializer+';')
    constructors='\n'.join(m['source'] for m in c['constructors'])+'\n'+'\n'.join(c['restored'])
    (folder/(name+'.java')).write_text('package '+pkg+'; public '+('abstract ' if c['flags']&0x400 else '')+'class '+name+' extends '+ty(c['parent'])+' {\n'+'\n'.join(declarations)+'\n'+constructors+'\n}\n')
    for m in c['constructors']:source_hashes.append({'name':c['name'],'args':m['args'],'sha256':hashlib.sha256(m['source'].encode()).hexdigest()})
    for source in c['restored']:restored.append({'name':c['name'],'source':source,'sha256':hashlib.sha256(source.encode()).hexdigest()})
    fields='Map.ofEntries('+','.join('Map.entry('+k+',new FieldRef('+j(v['owner'])+','+j(v['name'])+','+j(v['type'])+'))' for k,v in c['fields'].items())+')'
    strings='Map.ofEntries('+','.join('Map.entry('+k+','+j(v)+')' for k,v in c['strings'].items())+')'
    types='Map.ofEntries('+','.join('Map.entry('+k+','+j(v)+')' for k,v in c['types'].items())+')'
    calls='Map.ofEntries('+','.join('Map.entry('+k+',new CallRef('+j(v['owner'])+',new String[]{'+','.join(j(a) for a in v['args'])+'}))' for k,v in c['calls'].items())+')'
    for m in c['constructors']:
        rows.append('new Fixture('+j(c['name'])+',new String[]{'+','.join(j(a) for a in m['args'])+'},'+str(m['registers'])+','+str(m['ins'])+',new int[]{'+','.join(map(str,m['words']))+'},'+fields+','+strings+','+types+','+calls+')')
if data['kind']=='phonepe':
    for pkg,name in [('ga2','c'),('s62','e0')]:
        folder=out/pkg;folder.mkdir(exist_ok=True);(folder/(name+'.java')).write_text('package '+pkg+'; public class '+name+' {}\n')
oracle=r'''import java.util.*;import java.lang.reflect.*;
public class Oracle {
 record FieldRef(String owner,String name,String type) {String key(){return owner+"."+name;}}
 record CallRef(String owner,String[] args) {}
 record Fixture(String name,String[] args,int registers,int ins,int[] words,Map<Integer,FieldRef> fields,Map<Integer,String> strings,Map<Integer,String> types,Map<Integer,CallRef> calls) {}
 static Fixture[] fixtures={FIXTURES};
 static Class<?> type(String d)throws Exception{return switch(d){case "I"->int.class;case "J"->long.class;case "Z"->boolean.class;default->Class.forName(d.substring(1,d.length()-1).replace('/','.'));};}
 static int number(Object o){return o instanceof Boolean b?(b?1:0):((Number)o).intValue();}
 static Object sentinel()throws Exception{return Class.forName("kotlin.jvm.internal.e").getField("NO_RECEIVER").get(null);}
 static Fixture lookup(String owner,String[] args){String n=owner.substring(1,owner.length()-1).replace('/','.');return Arrays.stream(fixtures).filter(f->f.name.equals(n)&&Arrays.equals(f.args,args)).findFirst().orElseThrow();}
 static int execute(Fixture f,Object marker,Object[] args,Map<String,Object> state,int depth)throws Exception{
  if(depth>16)throw new AssertionError("depth");Object[] r=new Object[f.registers];int first=f.registers-f.ins;r[first]=marker;int reg=first+1;for(int n=0;n<args.length;n++){r[reg]=args[n];reg+=f.args[n].equals("J")?2:1;}int steps=0,calls=0;
  for(int pc=0;pc<f.words.length;){if(++steps>512)throw new AssertionError("steps");int w=f.words[pc],op=w&255;switch(op){
   case 0x01,0x07->{r[(w>>8)&15]=r[(w>>12)&15];pc++;}
   case 0x12->{int literal=w>>12;if(literal>=8)literal-=16;r[(w>>8)&15]=literal;pc++;}
   case 0x1a->{r[w>>8]=f.strings.get(f.words[pc+1]);pc+=2;}
   case 0x1c->{r[w>>8]=type(f.types.get(f.words[pc+1]));pc+=2;}
   case 0x62->{FieldRef field=f.fields.get(f.words[pc+1]);if(!field.name.equals("NO_RECEIVER"))throw new AssertionError(field);r[w>>8]=sentinel();pc+=2;}
   case 0x59,0x5a,0x5b,0x5c->{FieldRef field=f.fields.get(f.words[pc+1]);if(r[(w>>12)&15]!=marker)throw new AssertionError("store receiver");Object value=r[(w>>8)&15];if(field.type.equals("Z"))value=number(value)!=0;if(field.type.startsWith("L")&&value instanceof Integer n&&n==0)value=null;state.put(field.key(),value);pc+=2;}
   case 0x28->{int offset=(byte)(w>>8);pc+=offset;}
   case 0x33->{pc+=number(r[(w>>8)&15])!=number(r[(w>>12)&15])?(short)f.words[pc+1]:2;}
   case 0x38->{pc+=number(r[w>>8])==0?(short)f.words[pc+1]:2;}
   case 0xdd->{int pair=f.words[pc+1];r[w>>8]=number(r[pair&255])&(byte)(pair>>8);pc+=2;}
   case 0x70,0x76->{CallRef call=f.calls.get(f.words[pc+1]);int count=op==0x70?w>>12:w>>8;int[] words=new int[count];if(op==0x70){int packed=f.words[pc+2];for(int i=0;i<Math.min(count,4);i++)words[i]=(packed>>(4*i))&15;if(count==5)words[4]=(w>>8)&15;}else{for(int i=0;i<count;i++)words[i]=f.words[pc+2]+i;}if(r[words[0]]!=marker)throw new AssertionError("call receiver");Object[] values=new Object[call.args.length];int wi=1;for(int i=0;i<values.length;i++){values[i]=r[words[wi]];if(call.args[i].startsWith("L")&&values[i]instanceof Integer n&&n==0)values[i]=null;wi+=call.args[i].equals("J")?2:1;}if(wi!=count)throw new AssertionError("argument words");if(!call.owner.equals("Ljava/lang/Object;"))calls+=execute(lookup(call.owner,call.args),marker,values,state,depth+1);calls++;pc+=3;}
   case 0x0e->{if(pc+1!=f.words.length)throw new AssertionError("return");pc++;}
   default->throw new AssertionError("opcode "+Integer.toHexString(op));
  }}return calls;
 }
 static Object argument(String d,int seed,int i)throws Exception {int variant=Math.floorMod(seed+i,7);if(d.equals("I"))return new int[]{0,1,-1,Integer.MIN_VALUE,Integer.MAX_VALUE,2,3}[variant];if(d.equals("J"))return new long[]{0,1,-1,Long.MIN_VALUE,Long.MAX_VALUE,4294967297L,-4294967297L}[variant];if(variant==0)return null;if(d.equals("Ljava/lang/String;"))return new String(variant%2==0?"":("value"+i));if(d.equals("Ljava/lang/Class;"))return new Class<?>[]{Object.class,String.class,int.class,Oracle.class}[variant%4];if(d.equals("Ls62/e0;"))return type(d).getConstructor().newInstance();return new Object();}
 static int check(Fixture f,int seed)throws Exception {Object[] args=new Object[f.args.length];Class<?>[] types=new Class<?>[args.length];for(int i=0;i<args.length;i++){args[i]=argument(f.args[i],seed,i);types[i]=type(f.args[i]);}Map<String,Object> expected=new LinkedHashMap<>();int calls=execute(f,new Object(),args,expected,0);if(calls<1)throw new AssertionError("calls");Object actual=Class.forName(f.name).getDeclaredConstructor(types).newInstance(args);for(var entry:expected.entrySet()){int split=entry.getKey().lastIndexOf('.');Class<?> owner=type(entry.getKey().substring(0,split));Field field=owner.getDeclaredField(entry.getKey().substring(split+1));field.setAccessible(true);Object value=field.get(actual);if(field.getType().isPrimitive()){if(!value.equals(entry.getValue()))throw new AssertionError(f.name+" "+entry+" "+value);}else if(value!=entry.getValue())throw new AssertionError(f.name+" identity "+entry);}return 1;}
 public static void main(String[] a)throws Exception {Set<String> targets=Set.of(TARGETS);int count=0;for(Fixture f:fixtures)if(targets.contains(f.name))for(int seed=0;seed<2048;seed++)count+=check(f,seed);System.out.println("PASS "+count+" exact raw DEX/JVM layered constructor comparisons");}
}'''.replace('FIXTURES',',\n'.join(rows)).replace('TARGETS',','.join(j(n) for n in data['targets']))
(out/'Oracle.java').write_text(oracle)
result=subprocess.run([str(jdk/'javac'),'-d',str(out),*[str(p) for p in out.rglob('*.java')]],capture_output=True,text=True);(out/'javac.log').write_text(result.stdout+result.stderr);assert result.returncode==0,result.stderr
compiled=[]
for c in data['classes']:
    result=subprocess.run([str(jdk/'javap'),'-c','-p','-classpath',str(out),c['name']],capture_output=True,text=True,check=True);(out/(c['name']+'.javap.txt')).write_text(result.stdout)
    compiled.append({'name':c['name'],'javap_sha256':hashlib.sha256(result.stdout.encode()).hexdigest()})
    def bytecode_body(args):
        types=[ty(t) for t in args]
        signature=c['name']+'('+', '.join(types)+');'
        needle=result.stdout.find(signature);assert needle>=0,(signature,result.stdout)
        return result.stdout[needle:].split('Code:',1)[1].split('\n\n',1)[0]
    order=[]
    for m in c['constructors']:
        body=bytecode_body(m['args']);actual=[]
        for line in body.splitlines():
            if 'putfield' in line:actual.append('store:'+re.search(r'// Field (?:[^ ]*\.)?(\w+):',line).group(1))
            if 'getstatic' in line:actual.append('readStatic:'+re.search(r'// Field (?:[^ ]*\.)?(\w+):',line).group(1))
            if 'invokespecial' in line:
                match=re.search(r'// Method (?:([^ ]+)\.)?"<init>"',line);assert match,line
                actual.append('call:'+(match.group(1).replace('/','.') if match.group(1) else c['name']))
        expected=[];pc=0;words=m['words']
        while pc<len(words):
            op=words[pc]&255
            if op in [0x59,0x5a,0x5b,0x5c]:expected.append('store:'+c['fields'][str(words[pc+1])]['name'])
            if op==0x62:expected.append('readStatic:'+c['fields'][str(words[pc+1])]['name'])
            if op in [0x70,0x76]:
                owner=c['calls'][str(words[pc+1])]['owner']
                if owner not in [c['owner'],c['parent']]:
                    parent=c['parent'];seen=set()
                    while parent!=owner:
                        assert parent not in seen;seen.add(parent)
                        layer=next(x for x in data['classes'] if x['owner']==parent)
                        call_args=c['calls'][str(words[pc+1])]['args']
                        if not call_args:
                            empty=next(m for m in layer['constructors'] if not m['args'])
                            assert len(empty['words'])==4 and empty['words'][0]==0x1070 and empty['words'][2:]==[0,14],(layer['name'],empty)
                        else:assert layer['restored'],(c['name'],parent,owner)
                        parent=layer['parent']
                    owner=c['parent']
                expected.append('call:'+owner[1:-1].replace('/','.'))
            pc+=3 if op in [0x70,0x76] else 2 if op in [0x1a,0x1c,0x62,0x59,0x5a,0x5b,0x5c,0x33,0x38,0xdd] else 1
        assert actual==expected,(c['name'],m['args'],expected,actual)
        order.append({'args':m['args'],'order':actual})
    compiled[-1]['retained_effect_order']=order
    for restored_source in c['restored']:
        match=re.search(r'public \w+\(([^)]*)\)',restored_source);args=match.group(1);types=[a.strip().rsplit(' ',1)[0] for a in args.split(',')] if args else []
        types=[('java.lang.'+t) if t in ['Object','Class','String'] else t for t in types]
        signature=c['name']+'('+', '.join(types)+');';needle=result.stdout.find(signature);assert needle>=0,(signature,result.stdout)
        body=result.stdout[needle:].split('Code:',1)[1].split('\n\n',1)[0];ops=[re.sub(r'^\s*\d+:\s*','',line).split()[0] for line in body.splitlines() if re.match(r'\s*\d+:',line)]
        assert ops[-2:]==['invokespecial','return'],(signature,ops)
        assert all(op.startswith(('aload','iload','lload')) for op in ops[:-2]),(signature,ops)
result=subprocess.run([str(jdk/'java'),'-cp',str(out),'Oracle'],capture_output=True,text=True);(out/'jvm.log').write_text(result.stdout+result.stderr);assert result.returncode==0,result.stderr
proof={'kind':data['kind'],'jvm':result.stdout.strip(),'unchanged_retained_constructor_sources':source_hashes,'empty_restored_wrappers':restored,'compiled':compiled,'model':'Exact unchanged emitted constructor bodies and field declarations; unrelated methods/interfaces omitted; NO_RECEIVER static sentinel and unrelated class defaults modelled'}
(base/'evidence.json').write_text(json.dumps(proof,indent=2)+'\n');print(json.dumps({'jvm':proof['jvm'],'sources':len(source_hashes),'restored':len(restored),'evidence':str(base/'evidence.json')},indent=2))
