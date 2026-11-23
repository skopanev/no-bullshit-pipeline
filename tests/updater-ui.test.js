import { test } from 'bun:test';
import { execFileSync } from 'node:child_process';

test('real UI wiring keeps cached banner offline, never auto-installs and renders manual progress/errors', () => {
  // Isolated JS process avoids leaking Tauri/global mocks into audio tests.
  execFileSync(process.execPath, ['--eval', `
    import assert from 'node:assert/strict';
    const elements = new Map(['update-banner', 'update-banner-text', 'update-download-btn', 'update-feedback', 'update-progress'].map(id => [id, {hidden:true, textContent:'', removeAttribute(){}}]));
    const handlers = new Map(), timers = [];
    const available = {version:'0.7.9', url:'https://github.com/skopanev/no-bullshit-pipeline/releases'};
    const cache = new Map([['nbp.availableUpdate.v1', JSON.stringify(available)]]);
    globalThis.localStorage = { getItem:key=>cache.get(key), setItem:(key,value)=>cache.set(key,value), removeItem:key=>cache.delete(key) };
    globalThis.document = {hidden:false, getElementById:id=>elements.get(id)};
    globalThis.setInterval = callback => {timers.push(callback); return timers.length;};
    globalThis.clearInterval = () => {};
    let installs = 0, eventCallback, failInstall, request;
    globalThis.window = {
      addEventListener:(name,callback)=>handlers.set(name,callback), removeEventListener:name=>handlers.delete(name),
      __TAURI__: {
        core:{invoke:async (command,args)=>{
          if(command === 'check_for_updates') throw new Error('offline');
          if(command === 'get_app_update_status') return {busy:false};
          if(command === 'install_app_update') {
            installs++; request=args;
            assert.equal(typeof eventCallback,'function');
            await new Promise((_,reject)=>{failInstall=reject;});
          }
        }},
        event:{listen:async (_,callback)=>{eventCallback=callback; return ()=>{eventCallback=null;};}}, dialog:{open:()=>{}}
      }
    };
    const state = await import('./src/js/core/state.js');
    const {initUpdateChecks} = await import('./src/js/updater.js');
    initUpdateChecks('0.7.8');
    await Bun.sleep(0);
    assert.equal(elements.get('update-banner').hidden,false);
    assert.match(elements.get('update-banner-text').textContent,/0.7.9/);
    handlers.get('focus')();
    await Bun.sleep(0);
    assert.equal(installs,0);
    state.setIsRecording(true);
    timers[0]();
    assert.equal(elements.get('update-download-btn').disabled,true);
    elements.get('update-download-btn').onclick();
    await Bun.sleep(0);
    assert.equal(installs,0);
    state.setIsRecording(false);
    timers[0]();
    await Bun.sleep(0);
    assert.equal(elements.get('update-download-btn').disabled,false);
    elements.get('update-download-btn').onclick();
    await Bun.sleep(0);
    assert.equal(installs,1);
    eventCallback({payload:{request_id:request.requestId,phase:'downloading',downloaded:25,total:100}});
    assert.equal(elements.get('update-progress').hidden,false);
    assert.equal(elements.get('update-progress').value,25);
    assert.match(elements.get('update-feedback').textContent,/25%/);
    failInstall(new Error('Signature rejected'));
    await Bun.sleep(0);
    assert.equal(elements.get('update-banner').hidden,false);
    assert.equal(elements.get('update-download-btn').textContent,'RETRY');
    assert.equal(elements.get('update-download-btn').disabled,false);
    assert.match(elements.get('update-feedback').textContent,/Signature rejected/);
    assert.equal(eventCallback,null);
    handlers.get('pagehide')();
  `], { cwd: process.cwd(), stdio: 'pipe', timeout: 10000 });
});
