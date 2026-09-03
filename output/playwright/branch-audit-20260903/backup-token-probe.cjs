const { request } = require('@playwright/test');
const fs = require('node:fs');
const path = require('node:path');
const out=__dirname;
const meta=JSON.parse(fs.readFileSync(path.join(out,'instance.json')));
const checks=[];
function check(name,condition,detail) { if(!condition) throw new Error(name+': '+detail); checks.push({name,detail}); console.log('PASS',name,detail??''); }
(async()=>{
 const admin=await request.newContext({storageState:path.join(out,'manual-browser-state.json')});
 const anon=await request.newContext();
 try {
  const panel=async()=>await (await admin.get(meta.url+'/admin/panel')).text();
  let html=await panel();
  const csrf=html.match(/name="_csrf"\s+value="([^"]+)"/)[1];
  const post=async(endpoint,form)=>admin.post(meta.url+endpoint,{form:{_csrf:csrf,...form},maxRedirects:0});
  let r=await post('/admin/backup/settings',{auto_full_backup_interval_hours:'0',auto_full_backup_copies_to_keep:'2',auto_full_backup_storage_mode:'directory',auto_full_backup_split_zip_part_size_gib:'1'});
  check('save retention of two full backups through admin HTTP',r.status()===303,r.status());
  const refs=[];
  for(let i=0;i<3;i++) {
   r=await post('/admin/backup/create',{storage_mode:'directory'});
   check('create full backup '+(i+1),r.status()===303,r.status());
   html=await panel();
   const all=[...html.matchAll(/<form[^>]*action="\/admin\/backup\/extract-board"[\s\S]*?<\/form>/g)].map(m=>m[0].match(/name="filename"\s+value="([^"]+)"/)[1]);
   const newRef=all.find(ref=>!refs.includes(ref));
   check('new backup appears immediately in listing '+(i+1),!!newRef,all.length);
   refs.push(newRef);
   if(i===2)check('retention invalidates listing and keeps only latest two',all.length===2 && !all.includes(refs[0]) && all.includes(refs[1]) && all.includes(refs[2]));
  }
  const ref=refs[2];
  const backupRoot=path.join(meta.data,'backups',ref);
  const manifest=JSON.parse(fs.readFileSync(path.join(backupRoot,'manifest.json')));
  check('saved manifest includes representative boards', ['audit','adult','combo'].every(b=>manifest.included_boards.some(row=>row.short_name===b)));
  check('retention removed oldest backup from disk',!fs.existsSync(path.join(meta.data,'backups',refs[0])));
  r=await post('/admin/backup/extract-board',{filename:ref,board_short:'combo',action:'download'});
  const location=r.headers().location;
  check('board extraction issues temporary download URL',r.status()===303 && location.includes('/temp-board/') && location.includes('token='));
  const url=new URL(location,meta.url);
  let invalid=new URL(url);invalid.searchParams.delete('token');
  check('temporary download denies missing token',(await admin.get(invalid.href,{maxRedirects:0})).status()===403);
  invalid.searchParams.set('token','wrong-token');
  check('temporary download denies wrong token',(await admin.get(invalid.href,{maxRedirects:0})).status()===403);
  check('temporary download also requires admin session',(await anon.get(url.href,{maxRedirects:0})).status()===403);
  r=await admin.get(url.href,{maxRedirects:0});
  const body=await r.body();
  check('authorized temporary backup downloads as ZIP',r.status()===200 && body.subarray(0,2).toString()==='PK' && r.headers()['content-type'].includes('application/zip'),body.length);
  fs.writeFileSync(path.join(out,'extracted-combo.zip'),body);
  check('temporary download token is single-use',(await admin.get(url.href,{maxRedirects:0})).status()===403);
  const filename=decodeURIComponent(url.pathname.split('/').pop());
  check('consumed temporary ZIP and token are cleaned up', !fs.existsSync(path.join(meta.data,'runtime/tmp/board-downloads',filename)) && !fs.existsSync(path.join(meta.data,'runtime/tmp/board-downloads',filename+'.token')));
  const progress=await admin.get(meta.url+'/admin/backup/progress');
  check('backup progress remains available',progress.status()===200);
  fs.writeFileSync(path.join(out,'backup-token-results.json'),JSON.stringify({status:'PASS',checks,retained:refs.slice(1)},null,2));
 } finally {await admin.dispose();await anon.dispose();}
})().catch(error=>{console.error(error);process.exitCode=1;});
