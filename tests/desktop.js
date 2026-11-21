(async () => {
  if (window.__testingDesktop || !window.__TAURI__ || document.readyState !== 'complete') return;
  window.__testingDesktop = true;
  const result = { ok: false, steps: [] };
  const ipc = window.__TAURI__.core.invoke;
  const assert = (ok, reason) => { if (!ok) throw Error(reason); };
  try {
    for (let i = 0; i < 100 && !S.overview; i++) await new Promise(r => setTimeout(r, 100));
    assert(S.overview?.hardware.memory.total_bytes > 0, 'missing hardware data');
    assert(document.querySelector('#main h1')?.textContent === window.tr(S.overview.conclusion.headline), `overview failed to render: page=${S.page}, actual=${document.querySelector('#main h1')?.textContent}, expected=${window.tr(S.overview.conclusion.headline)}`);
    result.steps.push('overview rendered');
    const settings = await ipc('get_settings');
    settings.port = 18686;
    await ipc('save_settings', {settings});
    assert((await ipc('get_settings')).port === 18686, 'settings round trip');
    const previousModelDirState = {
      call, loadOverview, settings: S.settings, overview: S.overview,
      engine: S.engine, catalog: S.catalog, local: S.local, page: S.page,
    };
    const selectedModelDir = 'D:\\LocMdRegressionModels';
    let pickerCalls = 0;
    let savedModelDir = '';
    const interceptedCall = call = async (cmd, args) => {
      if (cmd === 'pick_folder') {
        pickerCalls++;
        return selectedModelDir;
      }
      if (cmd === 'save_settings') {
        savedModelDir = args.settings.models_dir;
        S.settings = { ...args.settings };
        return S.settings;
      }
      throw Error(`unexpected model-directory test command: ${cmd}`);
    };
    assert(call === interceptedCall, 'could not intercept model-directory IPC safely');
    const interceptedLoadOverview = loadOverview = async () => {
      S.overview = { ...S.overview, models_dir: savedModelDir };
      if (S.page === 'overview') render();
    };
    assert(loadOverview === interceptedLoadOverview, 'could not intercept overview refresh safely');
    try {
      S.page = 'overview';
      S.settings = { ...S.settings, models_dir: 'C:\\LocMdOldModels' };
      const disk = S.overview.hardware.disks[0] || { available_bytes: 1, is_ssd: true };
      S.overview = {
        ...S.overview,
        models_dir: 'C:\\LocMdOldModels',
        hardware: {
          ...S.overview.hardware,
          disks: [
            { ...disk, mount: 'C:\\', available_bytes: 100 },
            { ...disk, mount: 'D:\\', available_bytes: 200 },
          ],
        },
      };
      render();
      const changeModelDir = document.querySelector('#main a[data-act="changeModelDir"]');
      assert(changeModelDir?.textContent === '更改', 'overview model disk change link');
      changeModelDir.click();
      for (let i = 0; i < 50 && savedModelDir !== selectedModelDir; i++) await new Promise(r => setTimeout(r, 20));
      assert(pickerCalls === 1, 'model disk change did not open the folder picker');
      assert(savedModelDir === selectedModelDir && S.settings.models_dir === selectedModelDir, 'model disk change did not save the selected directory');
      assert(S.overview.models_dir === selectedModelDir, 'model disk change did not refresh the overview');
      assert(document.querySelector('#main .specs')?.textContent.includes('D:'), 'overview still shows the previous model disk');
      result.steps.push('overview model disk picker, save and refresh');
    } finally {
      call = previousModelDirState.call;
      loadOverview = previousModelDirState.loadOverview;
      S.settings = previousModelDirState.settings;
      S.overview = previousModelDirState.overview;
      S.engine = previousModelDirState.engine;
      S.catalog = previousModelDirState.catalog;
      S.local = previousModelDirState.local;
      S.page = previousModelDirState.page;
      render();
    }
    const readySettings = S.settings;
    S.settings = null;
    actions.go({page:'settings'}, null, {preventDefault() {}});
    assert(!document.getElementById('st-dir'), 'settings loading placeholder precondition');
    applyLoadedSettings(readySettings);
    assert(document.getElementById('st-dir')?.value === readySettings.models_dir, 'settings page did not refresh after model directory settings loaded');
    result.steps.push('settings page refresh after delayed settings load');
    go('overview');
    const catalog = await ipc('get_catalog');
    assert(catalog.length > 0, 'empty catalog');
    go('models');
    await loadCatalog();
    assert(document.querySelector('#main').textContent.includes(window.tr('导入本地 GGUF')), 'model page');
    S.settings = await ipc('get_settings');
    go('settings');
    assert(document.getElementById('st-port')?.value === '18686', 'settings page');
    go('agent');
    go('image');
    await loadImageData(true);
    assert(S.img.catalog.length === 12, 'image catalog');
    go('video');
    go('overview');
    result.steps.push('settings, catalog and navigation');
    const until = async (predicate, description) => {
      for (let i = 0; i < 150; i++) {
        if (predicate()) return;
        await new Promise(r => setTimeout(r,100));
      }
      throw Error(description);
    };
    await until(() => S.newVer?.version === '3.1.1', 'automatic update check');
    if (S.modal?.update) actions.laterUpdate();
    assert(!(await ipc('accel_status')).member, 'unexpected initial membership');
    await ipc('save_settings', {settings:{...S.settings, member_token:'unverified', member_expires:4102444800}});
    assert(!(await ipc('accel_status')).member, 'settings write granted membership');
    actions.openBuy();
    await until(() => S.modal?.plan === 'fixture', 'checkout plans');
    await actions.createOrder();
    assert(S.modal?.step === 'pay' && S.modal.qrSvg.includes('<svg'), 'QR checkout');
    assert(!(await ipc('accel_status')).member, 'unpaid order activated membership');
    await until(() => S.accel.member && S.modal?.step === 'done', 'paid order confirmation');
    closeModal();
    assert((await ipc('accel_status')).member, 'paid membership not persisted');
    let denied = false;
    try { await ipc('activate_code', {code:'invalid'}); } catch { denied = true; }
    assert(denied, 'invalid activation code accepted');
    assert((await ipc('activate_code', {code:'FIXTURE-CODE'})).member, 'valid activation failed');
    assert((await ipc('accel_recover', {email:'fixture@example.invalid'})).includes('受理'), 'recovery failed');
    result.steps.push('automatic update notice, checkout, pending then paid, invalid/valid activation, email recovery');
    const path = __MODEL_FIXTURE__;
    if (path) {
      const imported = await ipc('import_path', {path});
      assert((await ipc('list_local')).some(m => m.path === imported.path), 'import missing');
      S.run.path = imported.path; S.run.ctx = 256;
      await actions.startModel();
      const status = await ipc('server_status');
      assert(status.ready, 'model not ready');
      const response = await llmFetch({messages:[{role:'user',content:'Once upon a time'}],max_tokens:8,temperature:0},new AbortController().signal);
      const answer = await response.json();
      assert(response.ok && answer.choices?.length && answer.usage?.completion_tokens > 0, JSON.stringify(answer));
      await ipc('stop_model');
      assert(!(await ipc('server_status')).running, 'engine did not stop');
      result.steps.push('GGUF import, real chat completion, stop');
    }
    result.ok = true;
  } catch (e) { result.error = String(e); result.stack = e?.stack; }
  await window.__TAURI__.event.emit('desktop-test-result', result);
})();
