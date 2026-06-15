//! Transparent, click-through hint overlay rendered by a WebView2 hosted in
//! **composition mode** (the only mode that renders see-through over the
//! desktop — see the Decision log in CLAUDE.md).
//!
//! `WebViewOverlay` owns the WebView2 + DirectComposition objects but NOT the
//! host window or the message loop — `app.rs` owns those, so the overlay can be
//! created once at startup and reused (show/hide) on every hotkey press instead
//! of paying WebView2's cold-init cost each time.
//!
//! A static shell page is loaded once; `render()` pushes hint state into it via
//! `ExecuteScript`, so there is no per-activation navigation/flicker.

use anyhow::{anyhow, Result};
use std::sync::mpsc;

use windows::core::{Interface, HSTRING};
use windows::Win32::Foundation::{E_POINTER, HWND, RECT};
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP};
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_SDK_VERSION,
};
use windows::Win32::Graphics::DirectComposition::{
    DCompositionCreateDevice, IDCompositionDevice, IDCompositionTarget, IDCompositionVisual,
};
use windows::Win32::Graphics::Dxgi::IDXGIDevice;
use windows::Win32::Foundation::HMODULE;

use webview2_com::Microsoft::Web::WebView2::Win32::{
    CreateCoreWebView2Environment, ICoreWebView2, ICoreWebView2CompositionController,
    ICoreWebView2Controller, ICoreWebView2Controller2, ICoreWebView2Environment,
    ICoreWebView2Environment3, COREWEBVIEW2_COLOR,
};
use webview2_com::{
    CreateCoreWebView2CompositionControllerCompletedHandler,
    CreateCoreWebView2EnvironmentCompletedHandler, ExecuteScriptCompletedHandler,
};

/// One hint to draw: a label at a physical-pixel screen coordinate (center),
/// plus how many leading characters the hint code matches so far (for coloring,
/// used in hint-pick mode), and whether this is the currently-selected top match.
pub struct RenderItem {
    pub label: String,
    pub x: i32,
    pub y: i32,
    pub typed: usize,
    pub selected: bool,
    /// Anchor the label above `(x, y)` (y = element top) rather than centered on it.
    pub above: bool,
}

/// One row in the results list (the search bar's drop-down): a hint code, the
/// element's accessible name, and whether it's the current Enter target.
pub struct ListRow {
    pub label: String,
    pub name: String,
    pub selected: bool,
}

/// One resize handle to draw: a single-letter label at a physical-pixel screen
/// coordinate, and whether it's the currently grabbed handle. `splitter` marks a
/// pane-boundary handle (labels i+, drawn blue) versus a window handle (a–h,
/// yellow).
pub struct ResizeHandleItem {
    pub label: String,
    pub x: i32,
    pub y: i32,
    pub selected: bool,
    pub splitter: bool,
}

/// The resize HUD: the target window's current size and the grabbed handle's
/// label (if any) so the user can see what an arrow press will move.
pub struct ResizeHud {
    pub width: i32,
    pub height: i32,
    pub selected_label: Option<String>,
}

/// Owns the WebView2 composition objects for the overlay window. The COM
/// handles are kept alive for the struct's lifetime; dropping it tears them down.
pub struct WebViewOverlay {
    controller: ICoreWebView2Controller,
    webview: ICoreWebView2,
    // Kept alive; not otherwise referenced after setup.
    _composition_controller: ICoreWebView2CompositionController,
    _dcomp: IDCompositionDevice,
    _target: IDCompositionTarget,
    _root_visual: IDCompositionVisual,
    _d3d: ID3D11Device,
}

impl WebViewOverlay {
    /// Build the composition pipeline for `hwnd` and load the shell page.
    /// `vw`/`vh` are the window's client size in physical pixels.
    pub fn new(hwnd: HWND, vw: i32, vh: i32, debug: bool) -> Result<Self> {
        // SAFETY: graphics/COM setup; each call is checked. COM (STA) is
        // initialized by the caller on this thread.
        unsafe {
            // --- DirectComposition visual tree backing the WebView ---
            let d3d = create_d3d_device()?;
            let dxgi: IDXGIDevice = d3d.cast()?;
            let dcomp: IDCompositionDevice = DCompositionCreateDevice(&dxgi)?;
            let target: IDCompositionTarget = dcomp.CreateTargetForHwnd(hwnd, true)?;
            let root_visual: IDCompositionVisual = dcomp.CreateVisual()?;
            target.SetRoot(&root_visual)?;

            // --- WebView2 environment + composition controller ---
            let environment = create_environment()?;
            let env3: ICoreWebView2Environment3 = environment.cast()?;
            let composition_controller = create_composition_controller(&env3, hwnd)?;
            composition_controller.SetRootVisualTarget(&root_visual)?;

            // Transparent background so the desktop shows through.
            let controller2: ICoreWebView2Controller2 = composition_controller.cast()?;
            controller2.SetDefaultBackgroundColor(COREWEBVIEW2_COLOR {
                A: 0,
                R: 0,
                G: 0,
                B: 0,
            })?;

            let controller: ICoreWebView2Controller = composition_controller.cast()?;
            controller.SetBounds(RECT {
                left: 0,
                top: 0,
                right: vw,
                bottom: vh,
            })?;
            // Start hidden; app shows it on activation.
            controller.SetIsVisible(false)?;

            let webview = controller.CoreWebView2()?;
            webview.NavigateToString(&HSTRING::from(shell_html(debug)))?;

            dcomp.Commit()?;

            Ok(Self {
                controller,
                webview,
                _composition_controller: composition_controller,
                _dcomp: dcomp,
                _target: target,
                _root_visual: root_visual,
                _d3d: d3d,
            })
        }
    }

    /// Show or hide the WebView content.
    pub fn set_visible(&self, visible: bool) -> Result<()> {
        // SAFETY: simple property set on a live controller.
        unsafe { self.controller.SetIsVisible(visible)? };
        Ok(())
    }

    /// Replace the rendered UI state: the `floating` labels over each element,
    /// the current `query` text, the `mode_badge` ("BOTH"/"SEARCH"/"HINTS"), and
    /// the results list split into a `top` (hint) section and `bottom` (name
    /// search) section separated by the divider.
    pub fn render(
        &self,
        floating: &[RenderItem],
        query: &str,
        mode_badge: &str,
        top: &[ListRow],
        bottom: &[ListRow],
    ) -> Result<()> {
        let js = format!(
            "render({{float:{},q:\"{}\",mode:\"{}\",top:{},bot:{}}})",
            items_json(floating),
            json_escape_str(query),
            mode_badge, // fixed ASCII identifier, no escaping needed
            rows_json(top),
            rows_json(bottom),
        );
        // SAFETY: ExecuteScript with a no-op completion handler; fire-and-forget.
        unsafe {
            let handler = ExecuteScriptCompletedHandler::create(Box::new(|_err, _result| Ok(())));
            self.webview.ExecuteScript(&HSTRING::from(js), &handler)?;
        }
        Ok(())
    }

    /// Replace the rendered UI with the resize view: the eight `handles` over the
    /// target window's corners/edges, plus a `hud` showing its current size and
    /// the grabbed handle.
    pub fn render_resize(&self, handles: &[ResizeHandleItem], hud: &ResizeHud) -> Result<()> {
        let sel = match &hud.selected_label {
            Some(l) => format!("\"{}\"", json_escape_str(l)),
            None => String::from("null"),
        };
        let js = format!(
            "renderResize({{handles:{},w:{},h:{},sel:{}}})",
            resize_handles_json(handles),
            hud.width,
            hud.height,
            sel,
        );
        // SAFETY: ExecuteScript with a no-op completion handler; fire-and-forget.
        unsafe {
            let handler = ExecuteScriptCompletedHandler::create(Box::new(|_err, _result| Ok(())));
            self.webview.ExecuteScript(&HSTRING::from(js), &handler)?;
        }
        Ok(())
    }
}

/// Escape an arbitrary string for embedding inside a JSON/JS double-quoted
/// string literal. The search query is untrusted free text (it can contain
/// `"`, `\`, or control chars), so it must be escaped before reaching the JS
/// layer — unlike hint labels, which are `[a-z]` only.
fn json_escape_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

/// Serialize render items as a JSON array `[{l:"AB",x:1,y:2,t:1,s:0},...]`.
/// Labels are `[a-z]`/`[A-Z]` only, so no escaping is needed.
fn items_json(items: &[RenderItem]) -> String {
    let mut s = String::from("[");
    for (i, it) in items.iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        s.push_str(&format!(
            "{{l:\"{}\",x:{},y:{},t:{},s:{},a:{}}}",
            it.label.to_uppercase(),
            it.x,
            it.y,
            it.typed,
            it.selected as u8,
            it.above as u8
        ));
    }
    s.push(']');
    s
}

/// Serialize results-list rows as `[{l:"AA",n:"save file",s:0},...]`. The name
/// is untrusted (arbitrary accessible text) so it is JSON-escaped.
fn rows_json(rows: &[ListRow]) -> String {
    let mut s = String::from("[");
    for (i, r) in rows.iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        s.push_str(&format!(
            "{{l:\"{}\",n:\"{}\",s:{}}}",
            r.label.to_uppercase(),
            json_escape_str(&r.name),
            r.selected as u8
        ));
    }
    s.push(']');
    s
}

/// Serialize resize handles as `[{l:"A",x:1,y:2,s:0,p:0},...]`. `p` marks a
/// splitter handle. Labels carry only `[a-z]` plus an orientation glyph (↔/↕),
/// neither of which needs escaping.
fn resize_handles_json(items: &[ResizeHandleItem]) -> String {
    let mut s = String::from("[");
    for (i, it) in items.iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        s.push_str(&format!(
            "{{l:\"{}\",x:{},y:{},s:{},p:{}}}",
            it.label.to_uppercase(),
            it.x,
            it.y,
            it.selected as u8,
            it.splitter as u8
        ));
    }
    s.push(']');
    s
}

/// The static shell page: defines `render(items)` and styling. Coordinates are
/// physical pixels; the page divides by `devicePixelRatio` so labels land
/// correctly under any monitor DPI scaling.
fn shell_html(debug: bool) -> String {
    // In debug mode, tint the whole page so we can confirm the surface paints.
    let body_bg = if debug {
        "rgba(255,0,0,0.25)"
    } else {
        "transparent"
    };
    format!(
        r#"<!doctype html><html><head><meta charset="utf-8"><style>
html,body{{margin:0;padding:0;width:100%;height:100%;background:{body_bg};overflow:hidden;
  font-family:'Cascadia Code','Cascadia Mono',Consolas,'Courier New',monospace;}}
.hint{{position:fixed;transform:translate(-50%,-50%);background:#0d1117;
  border:1px solid #00e5cc;color:#00e5cc;font-size:12px;font-weight:700;
  padding:1px 5px;border-radius:3px;white-space:nowrap;
  box-shadow:0 1px 5px rgba(0,0,0,.6);}}
.hint.above{{transform:translate(-50%,calc(-100% - 3px));}}
.hint.sel{{background:#00e5cc;color:#0d1117;border-color:#fff;z-index:10;
  animation:hintpulse 1.1s ease-in-out infinite;}}
@keyframes hintpulse{{
  0%,100%{{box-shadow:0 0 0 0 rgba(0,229,204,.75),0 1px 6px rgba(0,0,0,.6);}}
  50%{{box-shadow:0 0 0 6px rgba(0,229,204,0),0 1px 6px rgba(0,0,0,.6);}}
}}
.typed{{color:#ff9500;}}
.hint.sel .typed{{color:#7a3d00;}}
.palette{{position:fixed;top:14px;left:50%;transform:translateX(-50%);
  background:#0d1117;border:1px solid #00e5cc;border-radius:8px;
  min-width:300px;max-width:520px;color:#e6edf3;overflow:hidden;
  box-shadow:0 4px 20px rgba(0,0,0,.6);}}
.hdr{{display:flex;align-items:center;padding:7px 12px;font-size:14px;white-space:nowrap;}}
.hdr .mode{{color:#0d1117;background:#00e5cc;font-weight:700;font-size:11px;
  padding:1px 7px;border-radius:3px;margin-right:10px;letter-spacing:.5px;}}
.palette.search .mode{{background:#7aa2ff;}}
.palette.hints .mode{{background:#ff9500;}}
.palette.resize{{border-color:#ffd166;}}
.palette.resize .mode{{background:#ffd166;}}
.hint.handle{{border-color:#ffd166;color:#ffd166;}}
.hint.handle.sel{{background:#ffd166;color:#0d1117;border-color:#fff;}}
.hint.handle.splitter{{border-color:#7aa2ff;color:#7aa2ff;}}
.hint.handle.splitter.sel{{background:#7aa2ff;color:#0d1117;border-color:#fff;}}
.hdr .q{{color:#e6edf3;white-space:pre;}}
.hdr .ph{{color:#6b7681;}}
.hdr .caret{{color:#00e5cc;}}
.list{{max-height:260px;overflow:hidden;padding-bottom:4px;}}
.row{{display:flex;align-items:center;padding:3px 12px;font-size:13px;}}
.row .code{{color:#00e5cc;font-weight:700;min-width:28px;}}
.row .name{{color:#c9d1d9;overflow:hidden;text-overflow:ellipsis;white-space:nowrap;}}
.row.sel{{background:#00e5cc;}}
.row.sel .code,.row.sel .name{{color:#0d1117;}}
.divider{{height:1px;margin:5px 16px;
  background:linear-gradient(to right,transparent,rgba(0,229,204,.55),transparent);}}
.legend{{display:flex;flex-wrap:wrap;gap:4px 14px;padding:5px 12px 6px;
  border-top:1px solid rgba(255,255,255,.05);font-size:11px;white-space:nowrap;}}
.legend .lg{{color:#6b7681;}}
.legend .k{{display:inline-block;background:#161b22;border:1px solid #30363d;
  border-radius:3px;padding:0 4px;margin-right:2px;color:#c9d1d9;font-size:10px;}}
</style></head><body><script>
const dpr=window.devicePixelRatio||1;
function mkRow(r){{
  const d=document.createElement('div'); d.className='row'+(r.s?' sel':'');
  const c=document.createElement('span'); c.className='code'; c.textContent=r.l;
  const n=document.createElement('span'); n.className='name';
  n.textContent=(r.n&&r.n.length)?r.n:'—';
  d.appendChild(c); d.appendChild(n); return d;
}}
// One legend entry: keycap(s) + a label, e.g. chip(['⇧','↵'],'right-click').
function chip(keys,label){{
  const w=document.createElement('span'); w.className='lg';
  for(const key of keys){{
    const k=document.createElement('span'); k.className='k'; k.textContent=key;
    w.appendChild(k);
  }}
  w.appendChild(document.createTextNode(label));
  return w;
}}
function mkLegend(){{
  const lg=document.createElement('div'); lg.className='legend';
  lg.appendChild(chip(['↵'],'select'));
  lg.appendChild(chip(['⇧','↵'],'right-click'));
  lg.appendChild(chip(['↑','↓'],'move'));
  lg.appendChild(chip(['⇥'],'mode'));
  return lg;
}}
function render(state){{
  const b=document.body; b.innerHTML='';
  const fl=state.float||[], top=state.top||[], bot=state.bot||[];

  // --- palette (search bar + results list) ---
  const pal=document.createElement('div');
  pal.className='palette '+(state.mode||'BOTH').toLowerCase();
  const hdr=document.createElement('div'); hdr.className='hdr';
  const m=document.createElement('span'); m.className='mode';
  m.textContent=state.mode||'BOTH'; hdr.appendChild(m);
  const q=state.q||'';
  if(q.length>0){{
    const qs=document.createElement('span'); qs.className='q'; qs.textContent=q;
    hdr.appendChild(qs);
  }} else {{
    const ph=document.createElement('span'); ph.className='ph';
    ph.textContent='type to search / hint'; hdr.appendChild(ph);
  }}
  const car=document.createElement('span'); car.className='caret';
  car.textContent='▌'; hdr.appendChild(car);
  pal.appendChild(hdr);
  pal.appendChild(mkLegend());

  let selRow=null;
  if(top.length || bot.length){{
    const list=document.createElement('div'); list.className='list';
    for(const r of top){{ const el=mkRow(r); if(r.s) selRow=el; list.appendChild(el); }}
    if(top.length && bot.length){{
      const dv=document.createElement('div'); dv.className='divider'; list.appendChild(dv);
    }}
    for(const r of bot){{ const el=mkRow(r); if(r.s) selRow=el; list.appendChild(el); }}
    pal.appendChild(list);
  }}
  b.appendChild(pal);
  // Keep the arrow-selected row visible in the capped, scrollable list.
  if(selRow) selRow.scrollIntoView({{block:'nearest'}});

  // --- floating labels over each element ---
  for(const h of fl){{
    const d=document.createElement('div');
    d.className='hint'+(h.s?' sel':'')+(h.a?' above':'');
    const t=h.t||0;
    if(t>0){{
      const s=document.createElement('span'); s.className='typed';
      s.textContent=h.l.slice(0,t); d.appendChild(s);
      d.appendChild(document.createTextNode(h.l.slice(t)));
    }} else {{ d.textContent=h.l; }}
    d.style.left=(h.x/dpr)+'px'; d.style.top=(h.y/dpr)+'px';
    b.appendChild(d);
  }}
}}
function renderResize(state){{
  const b=document.body; b.innerHTML='';
  const hs=state.handles||[];

  // --- HUD palette: current size + grabbed handle + legend ---
  const pal=document.createElement('div'); pal.className='palette resize';
  const hdr=document.createElement('div'); hdr.className='hdr';
  const m=document.createElement('span'); m.className='mode';
  m.textContent='RESIZE'; hdr.appendChild(m);
  const q=document.createElement('span'); q.className='q';
  q.textContent=state.w+' × '+state.h+'  '+(state.sel?('['+state.sel+']'):'pick a handle');
  hdr.appendChild(q);
  const car=document.createElement('span'); car.className='caret';
  car.textContent='▌'; hdr.appendChild(car);
  pal.appendChild(hdr);
  const lg=document.createElement('div'); lg.className='legend';
  lg.appendChild(chip(['A','…','H'],'grab'));
  lg.appendChild(chip(['I','…'],'splitters'));
  lg.appendChild(chip(['←','↑','↓','→'],'resize'));
  lg.appendChild(chip(['⇧'],'fine'));
  lg.appendChild(chip(['↵'],'commit'));
  lg.appendChild(chip(['esc'],'restore'));
  lg.appendChild(chip(['caps'],'exit'));
  pal.appendChild(lg);
  b.appendChild(pal);

  // --- handle pins over the target window's corners / edge midpoints, plus
  //     blue splitter handles on shared pane edges ---
  for(const h of hs){{
    const d=document.createElement('div');
    d.className='hint handle'+(h.p?' splitter':'')+(h.s?' sel':'');
    d.textContent=h.l;
    d.style.left=(h.x/dpr)+'px'; d.style.top=(h.y/dpr)+'px';
    b.appendChild(d);
  }}
}}
</script></body></html>"#,
        body_bg = body_bg
    )
}

/// Create a hardware D3D11 device, falling back to WARP (software) if needed.
unsafe fn create_d3d_device() -> Result<ID3D11Device> {
    for driver in [D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP] {
        let mut device: Option<ID3D11Device> = None;
        let hr = D3D11CreateDevice(
            None,
            driver,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT, // required for DirectComposition interop
            None,
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            None,
        );
        if hr.is_ok() {
            if let Some(d) = device {
                return Ok(d);
            }
        }
    }
    Err(anyhow!("failed to create a D3D11 device (hardware and WARP)"))
}

/// Create the WebView2 environment, pumping messages until the async op completes.
unsafe fn create_environment() -> Result<ICoreWebView2Environment> {
    let (tx, rx) = mpsc::channel();
    CreateCoreWebView2EnvironmentCompletedHandler::wait_for_async_operation(
        Box::new(|handler| {
            CreateCoreWebView2Environment(&handler).map_err(webview2_com::Error::WindowsError)
        }),
        Box::new(move |error_code, environment| {
            error_code?;
            let environment = environment.ok_or_else(|| windows::core::Error::from(E_POINTER))?;
            tx.send(environment).expect("send environment over channel");
            Ok(())
        }),
    )?;
    rx.recv()
        .map_err(|_| anyhow!("WebView2 environment creation did not complete"))
}

/// Create the composition controller bound to `hwnd`, pumping to completion.
unsafe fn create_composition_controller(
    env3: &ICoreWebView2Environment3,
    hwnd: HWND,
) -> Result<ICoreWebView2CompositionController> {
    let (tx, rx) = mpsc::channel();
    let env3 = env3.clone();
    CreateCoreWebView2CompositionControllerCompletedHandler::wait_for_async_operation(
        Box::new(move |handler| {
            env3.CreateCoreWebView2CompositionController(hwnd, &handler)
                .map_err(webview2_com::Error::WindowsError)
        }),
        Box::new(move |error_code, controller| {
            error_code?;
            let controller = controller.ok_or_else(|| windows::core::Error::from(E_POINTER))?;
            tx.send(controller).expect("send controller over channel");
            Ok(())
        }),
    )?;
    rx.recv()
        .map_err(|_| anyhow!("WebView2 composition controller creation did not complete"))
}
