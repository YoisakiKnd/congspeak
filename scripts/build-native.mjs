import { spawnSync } from 'node:child_process';
import { copyFileSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { createHash } from 'node:crypto';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
const root=resolve(dirname(fileURLToPath(import.meta.url)),'..');
const target='x86_64-pc-windows-msvc';
const args=process.platform==='win32'?['build','--release','-p','congspeak-native','--target',target]:['xwin','build','--release','-p','congspeak-native','--target',target];
const result=spawnSync('cargo',args,{cwd:root,stdio:'inherit',env:process.env});
if(result.error) throw result.error;
if(result.status!==0)process.exit(result.status??1);
const manifest=readFileSync(join(root,'apps/native/Cargo.toml'),'utf8');
const version=manifest.match(/^version = "([^"]+)"/m)[1];
const filename=`CongSpeak-Native-${version}-windows-x64.exe`;
const directory=join(root,'releases/windows');mkdirSync(directory,{recursive:true});
const destination=join(directory,filename);
copyFileSync(join(root,'target',target,'release','congspeak-native.exe'),destination);
const bytes=readFileSync(destination);
const pe=bytes.readUInt32LE(0x3c),optional=pe+24;
if(bytes.toString('ascii',0,2)!=='MZ'||bytes.readUInt32LE(pe)!==0x4550||bytes.readUInt16LE(pe+4)!==0x8664||bytes.readUInt16LE(optional)!==0x20b||bytes.readUInt16LE(optional+68)!==2)throw new Error('Expected an x64 PE32+ GUI executable');
const count=bytes.readUInt16LE(pe+6),sections=optional+bytes.readUInt16LE(pe+20);
function offset(rva){for(let i=0;i<count;i++){const s=sections+i*40,start=bytes.readUInt32LE(s+12),size=Math.max(bytes.readUInt32LE(s+8),bytes.readUInt32LE(s+16));if(rva>=start&&rva<start+size)return bytes.readUInt32LE(s+20)+rva-start;}throw new Error(`Unmapped PE address ${rva}`);}
const imports=[],rva=bytes.readUInt32LE(optional+120);
if(rva){for(let d=offset(rva);bytes.readUInt32LE(d+12);d+=20){const n=offset(bytes.readUInt32LE(d+12));imports.push(bytes.toString('ascii',n,bytes.indexOf(0,n)));}}
if(imports.some(n=>/^(vcruntime\d+(?:_\d+)?|msvcp\d+|WebView2Loader|opus|libopus(?:-\d+)?)\.dll$/i.test(n)))throw new Error(`Unexpected runtime DLL dependency: ${imports.join(', ')}`);
const sha256=createHash('sha256').update(bytes).digest('hex');writeFileSync(`${destination}.sha256`,`${sha256}  ${filename}\n`);
writeFileSync(join(directory,'native-build-info.json'),JSON.stringify({version,target,bytes:bytes.length,sha256,imports,webviewRequired:false,windowsRuntimeTested:false},null,2)+'\n');
console.log(`Portable native executable: ${destination}\nSize: ${(bytes.length/1024/1024).toFixed(2)} MiB\nSHA256: ${sha256}\nDLL imports: ${imports.join(', ')}`);
