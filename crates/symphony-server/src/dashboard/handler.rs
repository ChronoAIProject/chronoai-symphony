//! GET / dashboard handler.
//!
//! Renders a polished, self-contained HTML dashboard inspired by the OpenAI
//! Symphony reference implementation. Uses JavaScript polling (1s interval)
//! against `/api/v1/state` for live updates without full page reloads.

use std::sync::Arc;

use axum::extract::State;
use axum::response::Html;
use serde_json::Value;

use crate::router::AppState;

pub async fn dashboard(State(state): State<Arc<AppState>>) -> Html<String> {
    let snapshot = (state.snapshot_fn)();
    Html(render_shell(&snapshot))
}

fn render_shell(initial: &Value) -> String {
    let initial_json = serde_json::to_string(initial).unwrap_or_else(|_| "{}".into());
    format!(
        r##"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>Chrono AI Symphony</title>
<style>{CSS}</style>
</head>
<body>
<div class="app-shell">
<div id="dashboard" class="dashboard-shell"></div>
</div>
<script>
{JS}
let state = {initial_json};
render(state);
startPolling();
</script>
</body>
</html>"##
    )
}

const CSS: &str = r##"
:root {
  color-scheme: light;
  --page: #f7f7f8;
  --page-soft: #fbfbfc;
  --card: rgba(255,255,255,0.94);
  --card-muted: #f3f4f6;
  --ink: #202123;
  --muted: #6e6e80;
  --line: #ececf1;
  --line-strong: #d9d9e3;
  --accent: #10a37f;
  --accent-ink: #0f513f;
  --accent-soft: #e8faf4;
  --danger: #b42318;
  --danger-soft: #fef3f2;
  --warning-bg: #fff7e8;
  --warning-border: #f1d8a6;
  --warning-ink: #8a5a00;
  --shadow-sm: 0 1px 2px rgba(16,24,40,0.05);
  --shadow-lg: 0 20px 50px rgba(15,23,42,0.08);
}
*{box-sizing:border-box;margin:0;padding:0}
html{background:var(--page)}
body{
  min-height:100vh;
  background:
    radial-gradient(circle at top,rgba(16,163,127,0.12) 0%,rgba(16,163,127,0) 30%),
    linear-gradient(180deg,var(--page-soft) 0%,var(--page) 24%,#f3f4f6 100%);
  color:var(--ink);
  font-family:"Sohne","SF Pro Text","Helvetica Neue","Segoe UI",sans-serif;
  line-height:1.5;
}
a{color:var(--ink);text-decoration:none;transition:color 140ms ease}
a:hover{color:var(--accent)}
.app-shell{max-width:1280px;margin:0 auto;padding:2rem 1rem 3.5rem}
.dashboard-shell{display:grid;gap:1rem}

.hero-card,.section-card,.metric-card,.error-card{
  background:var(--card);border:1px solid rgba(217,217,227,0.82);
  box-shadow:var(--shadow-sm);backdrop-filter:blur(18px);
}
.hero-card{border-radius:28px;padding:clamp(1.25rem,3vw,2rem);box-shadow:var(--shadow-lg)}
.hero-grid{display:grid;grid-template-columns:minmax(0,1fr) auto;gap:1.25rem;align-items:start}
.eyebrow{color:var(--muted);text-transform:uppercase;letter-spacing:0.08em;font-size:0.76rem;font-weight:600}
.hero-title{margin:0.35rem 0 0;font-size:clamp(2rem,4vw,3.3rem);line-height:0.98;letter-spacing:-0.04em}
.hero-copy{margin:0.75rem 0 0;max-width:46rem;color:var(--muted);font-size:1rem}

.status-stack{display:grid;justify-items:end;align-content:start;min-width:min(100%,9rem)}
.status-badge{
  display:inline-flex;align-items:center;gap:0.45rem;min-height:2rem;
  padding:0.35rem 0.78rem;border-radius:999px;border:1px solid var(--line);
  background:var(--card-muted);color:var(--muted);font-size:0.82rem;font-weight:700;letter-spacing:0.01em;
}
.status-badge-dot{width:0.52rem;height:0.52rem;border-radius:999px;background:currentColor;opacity:0.9}
.status-badge-live{background:var(--accent-soft);border-color:rgba(16,163,127,0.18);color:var(--accent-ink)}
.status-badge-live .status-badge-dot{animation:pulse 2s ease-in-out infinite}
@keyframes pulse{0%,100%{opacity:1}50%{opacity:0.4}}

.metric-grid{display:grid;gap:0.85rem;grid-template-columns:repeat(auto-fit,minmax(180px,1fr))}
.metric-card{border-radius:22px;padding:1rem 1.05rem 1.1rem}
.metric-label{color:var(--muted);font-size:0.82rem;font-weight:600;letter-spacing:0.01em}
.metric-value{margin:0.35rem 0 0;font-size:clamp(1.6rem,2vw,2.1rem);line-height:1.05;letter-spacing:-0.03em;
  font-variant-numeric:tabular-nums slashed-zero}
.metric-detail{margin:0.45rem 0 0;color:var(--muted);font-size:0.88rem;
  font-variant-numeric:tabular-nums slashed-zero}

.section-card{border-radius:24px;padding:1.15rem}
.section-header{display:flex;justify-content:space-between;align-items:flex-start;gap:1rem;flex-wrap:wrap}
.section-title{font-size:1.08rem;line-height:1.2;letter-spacing:-0.02em}
.section-copy{margin:0.35rem 0 0;color:var(--muted);font-size:0.94rem}

.table-wrap{overflow-x:auto;margin-top:1rem}
.data-table{width:100%;border-collapse:collapse;table-layout:fixed;min-width:900px}
.data-table th{
  padding:0 0.5rem 0.75rem 0;text-align:left;color:var(--muted);
  font-size:0.78rem;font-weight:600;text-transform:uppercase;letter-spacing:0.04em;
}
.data-table td{
  padding:0.9rem 0.5rem 0.9rem 0;border-top:1px solid var(--line);
  vertical-align:top;font-size:0.94rem;
}
.issue-stack,.detail-stack,.token-stack{display:grid;gap:0.24rem;min-width:0}
.issue-id{font-weight:600;letter-spacing:-0.01em}
.issue-link{color:var(--muted);font-size:0.86rem}
.event-text{font-weight:500;line-height:1.45;overflow:hidden;text-overflow:ellipsis;white-space:nowrap}
.event-meta{color:var(--muted);overflow:hidden;text-overflow:ellipsis;white-space:nowrap}
.muted{color:var(--muted)}

.state-badge{
  display:inline-flex;align-items:center;min-height:1.85rem;padding:0.3rem 0.68rem;
  border-radius:999px;border:1px solid var(--line);background:var(--card-muted);
  color:var(--ink);font-size:0.8rem;font-weight:600;line-height:1;
}
.state-badge-active{background:var(--accent-soft);border-color:rgba(16,163,127,0.18);color:var(--accent-ink)}
.state-badge-warning{background:var(--warning-bg);border-color:var(--warning-border);color:var(--warning-ink)}
.state-badge-danger{background:var(--danger-soft);border-color:#f6d3cf;color:var(--danger)}

.code-panel{
  margin-top:1rem;padding:1rem;border-radius:18px;background:#f5f5f7;
  border:1px solid var(--line);color:#353740;font-size:0.9rem;white-space:pre-wrap;word-break:break-word;
  font-family:"Sohne Mono","SFMono-Regular","SF Mono",Consolas,monospace;
}
.empty-state{margin:1rem 0 0;color:var(--muted)}

.copy-btn{
  appearance:none;border:1px solid var(--line-strong);background:rgba(255,255,255,0.72);
  color:var(--muted);border-radius:999px;padding:0.34rem 0.72rem;cursor:pointer;
  font:inherit;font-size:0.82rem;font-weight:600;transition:background 140ms,color 140ms;
}
.copy-btn:hover{background:white;border-color:var(--muted);color:var(--ink)}

.refresh-btn{
  appearance:none;border:1px solid var(--accent);background:var(--accent);color:white;
  border-radius:999px;padding:0.5rem 1rem;cursor:pointer;font:inherit;font-weight:600;
  box-shadow:0 8px 20px rgba(16,163,127,0.18);transition:transform 140ms,box-shadow 140ms;
}
.refresh-btn:hover{transform:translateY(-1px);box-shadow:0 12px 24px rgba(16,163,127,0.22)}

.approve-btn{
  appearance:none;border:1px solid var(--accent);background:var(--accent);color:white;
  border-radius:999px;padding:0.34rem 0.72rem;cursor:pointer;font:inherit;
  font-size:0.82rem;font-weight:600;margin-right:0.4rem;transition:opacity 140ms;
}
.approve-btn:hover{opacity:0.9}
.deny-btn{
  appearance:none;border:1px solid var(--danger);background:var(--danger);color:white;
  border-radius:999px;padding:0.34rem 0.72rem;cursor:pointer;font:inherit;
  font-size:0.82rem;font-weight:600;transition:opacity 140ms;
}
.deny-btn:hover{opacity:0.9}
.activity-toggle{
  appearance:none;border:1px solid var(--line-strong);background:transparent;color:var(--muted);
  border-radius:999px;padding:0.2rem 0.6rem;cursor:pointer;font:inherit;
  font-size:0.78rem;font-weight:600;margin-top:0.3rem;transition:background 140ms,color 140ms;
}
.activity-toggle:hover{background:var(--card-muted);color:var(--ink)}
.activity-panel{
  margin-top:0.5rem;padding:0.75rem;border-radius:12px;background:#f5f5f7;
  border:1px solid var(--line);font-family:"Sohne Mono","SFMono-Regular","SF Mono",Consolas,monospace;
  font-size:0.82rem;max-height:300px;overflow-y:auto;white-space:pre-wrap;color:#353740;
}
.pending-badge{
  display:inline-flex;align-items:center;gap:0.35rem;padding:0.25rem 0.6rem;
  border-radius:999px;background:#fff3cd;border:1px solid #ffc107;color:#856404;
  font-size:0.78rem;font-weight:700;animation:pulse 2s ease-in-out infinite;
}

@media(max-width:860px){
  .app-shell{padding:1rem 0.85rem 2rem}
  .hero-grid{grid-template-columns:1fr}
  .status-stack{justify-items:start}
  .metric-grid{grid-template-columns:repeat(2,minmax(0,1fr))}
}
@media(max-width:560px){
  .metric-grid{grid-template-columns:1fr}
  .section-card,.hero-card{border-radius:20px;padding:1rem}
}
"##;

const JS: &str = r##"
function fmt(n){
  if(n==null)return"n/a";
  return n.toString().replace(/\B(?=(\d{3})+(?!\d))/g,",");
}
function fmtRuntime(s){
  if(s==null||s<0)return"0m 0s";
  s=Math.max(0,Math.floor(s));
  let m=Math.floor(s/60),sec=s%60;
  if(m>=60){let h=Math.floor(m/60);m=m%60;return h+"h "+m+"m "+sec+"s";}
  return m+"m "+sec+"s";
}
function timeSince(iso,now){
  if(!iso)return 0;
  let d=new Date(iso);
  return Math.max(0,(now-d)/1000);
}
function fmtTime(iso){
  if(!iso)return"";
  let d=new Date(iso);
  let h=String(d.getHours()).padStart(2,"0");
  let m=String(d.getMinutes()).padStart(2,"0");
  let s=String(d.getSeconds()).padStart(2,"0");
  return h+":"+m+":"+s;
}
function stateBadge(st){
  if(!st)return"state-badge";
  let s=st.toLowerCase();
  if(s.includes("progress")||s.includes("running")||s.includes("active"))return"state-badge state-badge-active";
  if(s.includes("blocked")||s.includes("error")||s.includes("failed"))return"state-badge state-badge-danger";
  if(s.includes("todo")||s.includes("queued")||s.includes("pending")||s.includes("retry"))return"state-badge state-badge-warning";
  return"state-badge";
}
function esc(s){if(!s)return"";let d=document.createElement("div");d.textContent=s;return d.innerHTML;}
function fmtRateLimits(rl){
  if(!rl||typeof rl!=="object")return"<p class=\"empty-state\">n/a</p>";
  let now=Date.now()/1000;
  function countdown(epochSecs){
    if(!epochSecs)return null;
    let diff=Math.max(0,Math.floor(epochSecs-now));
    if(diff<=0)return"now";
    let h=Math.floor(diff/3600),m=Math.floor((diff%3600)/60),s=diff%60;
    if(h>0)return h+"h "+m+"m";
    if(m>0)return m+"m "+s+"s";
    return s+"s";
  }
  function usedColor(pct){return pct<50?"var(--accent)":pct<80?"var(--warning-ink)":"var(--danger)";}
  function fmtWindow(mins){return mins>=1440?(mins/1440)+"d":mins>=60?(mins/60)+"h":mins+"m";}
  function renderTier(name,tier){
    if(!tier||typeof tier!=="object")return"";
    let used=tier.usedPercent!=null?tier.usedPercent:null;
    let resets=countdown(tier.resetsAt);
    let resetTime=tier.resetsAt?new Date(tier.resetsAt*1000).toLocaleTimeString():null;
    let window=tier.windowDurationMins?fmtWindow(tier.windowDurationMins):null;
    let html=`<article class="metric-card"><p class="metric-label">${esc(name)}</p>`;
    if(used!=null){
      html+=`<p class="metric-value" style="color:${usedColor(used)}">${used}% used</p>`;
    }
    let details=[];
    if(window)details.push(window+" window");
    if(resets)details.push("resets in "+resets);
    if(resetTime)details.push(resetTime);
    if(details.length)html+=`<p class="metric-detail">${details.join(" · ")}</p>`;
    html+=`</article>`;
    return html;
  }
  // Codex format: { rateLimits: { primary: {...}, secondary: {...}, limitId, planType } }
  let codexLimits=rl.rateLimits;
  if(codexLimits&&typeof codexLimits==="object"){
    let html='<div class="metric-grid" style="margin-top:1rem">';
    if(codexLimits.planType){
      html+=`<article class="metric-card"><p class="metric-label">Plan</p><p class="metric-value">${esc(codexLimits.planType)}</p>`;
      if(codexLimits.limitId)html+=`<p class="metric-detail">${esc(codexLimits.limitId)}</p>`;
      html+=`</article>`;
    }
    if(codexLimits.primary)html+=renderTier("Primary",codexLimits.primary);
    if(codexLimits.secondary)html+=renderTier("Secondary",codexLimits.secondary);
    if(codexLimits.credits!=null){
      html+=`<article class="metric-card"><p class="metric-label">Credits</p><p class="metric-value">${codexLimits.credits!=null?fmt(codexLimits.credits):"unlimited"}</p></article>`;
    }
    html+="</div>";
    return html;
  }
  // Claude format: { status, rateLimitType, resetsAt, overageStatus, overageResetsAt }
  let status=rl.status||rl.rateLimitType||"";
  let resetCountdown=countdown(rl.resetsAt);
  let resetTime=rl.resetsAt?new Date(rl.resetsAt*1000).toLocaleTimeString():null;
  let overageCountdown=countdown(rl.overageResetsAt);
  let overageTime=rl.overageResetsAt?new Date(rl.overageResetsAt*1000).toLocaleTimeString():null;
  let remaining=rl.remaining!=null?rl.remaining:null;
  let limit=rl.limit!=null?rl.limit:null;
  let html='<div class="metric-grid" style="margin-top:1rem">';
  if(status){
    let badgeClass=status==="allowed"?"state-badge state-badge-active":"state-badge state-badge-danger";
    html+=`<article class="metric-card"><p class="metric-label">Status</p><p class="metric-value"><span class="${badgeClass}">${esc(status)}</span></p></article>`;
  }
  if(rl.rateLimitType){
    html+=`<article class="metric-card"><p class="metric-label">Type</p><p class="metric-value">${esc(rl.rateLimitType)}</p></article>`;
  }
  if(resetCountdown){
    html+=`<article class="metric-card"><p class="metric-label">Resets in</p><p class="metric-value">${resetCountdown}</p>`;
    if(resetTime)html+=`<p class="metric-detail">${resetTime}</p>`;
    html+=`</article>`;
  }
  if(remaining!=null&&limit!=null){
    let pct=Math.round((remaining/limit)*100);
    let color=pct>50?"var(--accent)":pct>20?"var(--warning-ink)":"var(--danger)";
    html+=`<article class="metric-card"><p class="metric-label">Remaining</p><p class="metric-value" style="color:${color}">${fmt(remaining)} / ${fmt(limit)}</p></article>`;
  }
  if(rl.overageStatus){
    let overageBadge=rl.overageStatus==="allowed"?"state-badge state-badge-active":"state-badge state-badge-danger";
    html+=`<article class="metric-card"><p class="metric-label">Overage</p><p class="metric-value"><span class="${overageBadge}">${esc(rl.overageStatus)}</span></p>`;
    if(overageCountdown)html+=`<p class="metric-detail">Resets in ${overageCountdown} (${overageTime})</p>`;
    html+=`</article>`;
  }
  html+="</div>";
  if(!status&&remaining==null&&!rl.rateLimitType){
    html=`<pre class="code-panel">${JSON.stringify(rl,null,2)}</pre>`;
  }
  return html;
}
function copyText(text,btn){
  function done(){btn.textContent="Copied";setTimeout(()=>btn.textContent="Copy ID",1200);}
  function fallback(){
    let ta=document.createElement("textarea");ta.value=text;ta.style.position="fixed";ta.style.left="-9999px";ta.style.opacity="0";
    document.body.appendChild(ta);ta.focus();ta.select();
    try{document.execCommand("copy");done();}catch(e){}
    document.body.removeChild(ta);
  }
  try{
    if(navigator&&navigator.clipboard&&typeof navigator.clipboard.writeText==="function"&&window.isSecureContext){
      navigator.clipboard.writeText(text).then(done).catch(fallback);
    }else{fallback();}
  }catch(e){fallback();}
}

let expandedActivities={};
let activityScrollPos={};
function toggleActivity(issueId){
  expandedActivities[issueId]=!expandedActivities[issueId];
  if(expandedActivities[issueId]){activityScrollPos[issueId]=-1;}// -1 = scroll to bottom
  render(state);
}

async function approveAction(id,decision){
  try{
    await fetch("/api/v1/approve/"+encodeURIComponent(id),{
      method:"POST",
      headers:{"Content-Type":"application/json"},
      body:JSON.stringify({decision:decision})
    });
    let r=await fetch("/api/v1/state");
    if(r.ok){state=await r.json();render(state);}
  }catch(e){}
}

function render(data){
  let now=Date.now();
  let c=data.counts||{};
  let run=data.running||[];
  let ret=data.retrying||[];
  let tot=data.codex_totals||{};
  let rl=data.rate_limits;
  let approvals=data.pending_approvals||[];

  let activeSeconds=(tot.seconds_running||0);
  run.forEach(function(e){activeSeconds+=timeSince(e.started_at,now);});

  let html=`
  <header class="hero-card">
    <div class="hero-grid">
      <div>
        <p class="eyebrow">Chrono AI Symphony Observability</p>
        <h1 class="hero-title">Operations Dashboard</h1>
        <p class="hero-copy">Current state, retry pressure, token usage, and orchestration health for the active runtime.</p>
      </div>
      <div class="status-stack">
        <span class="status-badge status-badge-live">
          <span class="status-badge-dot"></span>Live
        </span>
        <button class="refresh-btn" onclick="forceRefresh()">Refresh</button>
      </div>
    </div>
  </header>

  <section class="metric-grid">
    <article class="metric-card">
      <p class="metric-label">Running</p>
      <p class="metric-value">${c.running||0}</p>
      <p class="metric-detail">Active issue sessions in the current runtime.</p>
    </article>
    <article class="metric-card">
      <p class="metric-label">Retrying</p>
      <p class="metric-value">${c.retrying||0}</p>
      <p class="metric-detail">Issues waiting for the next retry window.</p>
    </article>
    <article class="metric-card">
      <p class="metric-label">Total tokens</p>
      <p class="metric-value">${fmt(tot.total_tokens||0)}</p>
      <p class="metric-detail">In ${fmt(tot.input_tokens||0)} / Out ${fmt(tot.output_tokens||0)}</p>
    </article>
    <article class="metric-card">
      <p class="metric-label">Runtime</p>
      <p class="metric-value">${fmtRuntime(activeSeconds)}</p>
      <p class="metric-detail">Total across completed and active sessions.</p>
    </article>
  </section>

  <section class="section-card">
    <div class="section-header">
      <div>
        <h2 class="section-title">Rate limits</h2>
        <p class="section-copy">Latest upstream rate-limit snapshot, when available.</p>
      </div>
    </div>
    ${rl?fmtRateLimits(rl):"<p class=\"empty-state\">No rate limit data available.</p>"}
  </section>

  <section class="section-card">
    <div class="section-header">
      <div>
        <h2 class="section-title">Running sessions</h2>
        <p class="section-copy">Active issues, last known agent activity, and token usage.</p>
      </div>
    </div>`;

  if(run.length===0){
    html+=`<p class="empty-state">No active sessions.</p>`;
  }else{
    html+=`<div class="table-wrap"><table class="data-table">
    <colgroup>
      <col style="width:10rem"><col style="width:8rem"><col style="width:7.5rem">
      <col style="width:8.5rem"><col><col style="width:10rem">
    </colgroup>
    <thead><tr>
      <th>Issue</th><th>State</th><th>Session</th>
      <th>Runtime / turns</th><th>Agent update</th><th>Tokens</th>
    </tr></thead><tbody>`;
    run.forEach(function(e){
      let rt=fmtRuntime(timeSince(e.started_at,now));
      let turns=e.turn_count||0;
      let rtLabel=turns>0?rt+" / "+turns:rt;
      let lastMsg=esc(e.last_codex_message||e.last_message)||esc(e.last_codex_event||e.last_event||"n/a");
      let evMeta=esc(e.last_codex_event||e.last_event||"n/a");
      let evAt=e.last_codex_timestamp||e.last_event_at||"";
      let tIn=e.codex_input_tokens||0,tOut=e.codex_output_tokens||0,tTot=e.codex_total_tokens||0;
      if(e.tokens){tIn=e.tokens.input_tokens||tIn;tOut=e.tokens.output_tokens||tOut;tTot=e.tokens.total_tokens||tTot;}
      let st=e.state||(e.issue&&e.issue.state)||"";
      let id=esc(e.issue_identifier||e.identifier||"");
      let agentType=esc(e.agent_type||"codex");
      let agentBadge=agentType==="claude-cli"?"<span class=\"state-badge\" style=\"font-size:0.7rem;padding:0.15rem 0.45rem;margin-left:0.3rem;background:#f0e6ff;border-color:#c8a2f0;color:#6b21a8\">Claude</span>":"<span class=\"state-badge\" style=\"font-size:0.7rem;padding:0.15rem 0.45rem;margin-left:0.3rem\">Codex</span>";
      let issueKey=((e.issue_id||id)+(e.stage_role?"_"+e.stage_role:"")).replace(/[^a-zA-Z0-9_-]/g,"_");
      let activityLog=e.activity||e.activity_log||[];
      let hasActivity=activityLog.length>0;
      let stageRole=e.stage_role?`<span class="muted" style="font-size:0.78rem;margin-left:0.3rem">(${esc(e.stage_role)})</span>`:"";
      html+=`<tr>
        <td><div class="issue-stack"><span class="issue-id">${id}${agentBadge}${stageRole}</span>
          <a class="issue-link" href="/api/v1/${encodeURIComponent(id)}">JSON details</a>`;
      if(hasActivity){
        html+=`<button class="activity-toggle" onclick="toggleActivity('${esc(issueKey)}')">${expandedActivities[issueKey]?"Hide activity":"Activity ("+activityLog.length+")"}</button>`;
      }
      html+=`</div></td>
        <td><span class="${stateBadge(st)}">${esc(st)}</span></td>
        <td>${e.session_id?`<button class="copy-btn" onclick="copyText('${esc(e.session_id)}',this)">Copy ID</button>`:`<span class="muted">n/a</span>`}</td>
        <td>${rtLabel}</td>
        <td><div class="detail-stack"><span class="event-text" title="${esc(lastMsg)}">${lastMsg}</span>
          <span class="muted event-meta">${evMeta}${evAt?" &middot; "+esc(evAt):""}</span></div></td>
        <td><div class="token-stack">
          <span>Total: ${fmt(tTot)}</span>
          <span class="muted">In ${fmt(tIn)} / Out ${fmt(tOut)}</span></div></td>
      </tr>`;
      if(hasActivity&&expandedActivities[issueKey]){
        let logLines=activityLog.map(function(a){
          let ts=fmtTime(a.timestamp||a.ts||"");
          let evType=esc(a.event_type||a.type||"event");
          let msg=esc(a.message||a.msg||"");
          return"["+ts+"] "+evType+": "+msg;
        }).join("\n");
        html+=`<tr><td colspan="6"><div class="activity-panel" id="activity-${esc(issueKey)}">${logLines}</div></td></tr>`;
      }
    });
    html+=`</tbody></table></div>`;
  }
  html+=`</section>

  <section class="section-card">
    <div class="section-header">
      <div>
        <h2 class="section-title">Pending approvals</h2>
        <p class="section-copy">Agent requests waiting for operator decision.</p>
      </div>
    </div>`;

  if(approvals.length===0){
    html+=`<p class="empty-state">No pending approvals.</p>`;
  }else{
    html+=`<div class="table-wrap"><table class="data-table" style="min-width:700px">
    <thead><tr><th>Issue</th><th>Method</th><th>Requested at</th><th>Actions</th></tr></thead><tbody>`;
    approvals.forEach(function(a){
      let aId=esc(a.id||a.approval_id||"");
      let issue=esc(a.issue_identifier||a.issue_id||"");
      let method=esc(a.method||"n/a");
      let reqAt=esc(a.requested_at||a.created_at||"n/a");
      html+=`<tr>
        <td><div class="issue-stack"><span class="issue-id">${issue}</span></div></td>
        <td><span class="pending-badge">${method}</span></td>
        <td style="font-family:monospace">${reqAt}</td>
        <td>
          <button class="approve-btn" onclick="approveAction('${aId}','approve')">Approve</button>
          <button class="deny-btn" onclick="approveAction('${aId}','deny')">Deny</button>
        </td>
      </tr>`;
    });
    html+=`</tbody></table></div>`;
  }
  html+=`</section>

  <section class="section-card">
    <div class="section-header">
      <div>
        <h2 class="section-title">Retry queue</h2>
        <p class="section-copy">Issues waiting for the next retry window.</p>
      </div>
    </div>`;

  if(ret.length===0){
    html+=`<p class="empty-state">No issues are currently backing off.</p>`;
  }else{
    html+=`<div class="table-wrap"><table class="data-table" style="min-width:680px">
    <thead><tr><th>Issue</th><th>Attempt</th><th>Due at</th><th>Error</th></tr></thead><tbody>`;
    ret.forEach(function(e){
      let id=esc(e.issue_identifier||e.identifier||"");
      html+=`<tr>
        <td><div class="issue-stack"><span class="issue-id">${id}</span>
          <a class="issue-link" href="/api/v1/${encodeURIComponent(id)}">JSON details</a></div></td>
        <td>${e.attempt||"n/a"}</td>
        <td style="font-family:monospace">${esc(e.due_at||"n/a")}</td>
        <td>${esc(e.error||"n/a")}</td>
      </tr>`;
    });
    html+=`</tbody></table></div>`;
  }
  html+=`</section>`;

  // Save scroll positions of open activity panels before replacing DOM.
  document.querySelectorAll(".activity-panel").forEach(function(el){
    let key=el.id.replace("activity-","");
    if(key)activityScrollPos[key]=el.scrollTop;
  });

  document.getElementById("dashboard").innerHTML=html;

  // Restore scroll positions after DOM update.
  Object.keys(activityScrollPos).forEach(function(key){
    let el=document.getElementById("activity-"+key);
    if(el){
      if(activityScrollPos[key]===-1){
        el.scrollTop=el.scrollHeight;
        activityScrollPos[key]=el.scrollTop;
      }else{
        el.scrollTop=activityScrollPos[key];
      }
    }
  });
}

let pollTimer=null;
function startPolling(){
  if(pollTimer)clearInterval(pollTimer);
  pollTimer=setInterval(async function(){
    try{
      let r=await fetch("/api/v1/state");
      if(r.ok){state=await r.json();render(state);}
    }catch(e){}
  },1000);
}
async function forceRefresh(){
  try{
    await fetch("/api/v1/refresh",{method:"POST"});
    let r=await fetch("/api/v1/state");
    if(r.ok){state=await r.json();render(state);}
  }catch(e){}
}
"##;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn render_shell_contains_structure() {
        let snapshot = json!({
            "generated_at": "2026-03-17T10:00:00Z",
            "counts": { "running": 0, "retrying": 0 },
            "running": [],
            "retrying": [],
            "pending_approvals": [],
            "codex_totals": {
                "input_tokens": 0, "output_tokens": 0,
                "total_tokens": 0, "seconds_running": 0.0
            },
            "rate_limits": null
        });
        let html = render_shell(&snapshot);
        assert!(html.contains("Operations Dashboard"));
        assert!(html.contains("Chrono AI Symphony Observability"));
        assert!(html.contains("/api/v1/state"));
        assert!(html.contains("status-badge-live"));
        assert!(html.contains("Pending approvals"));
        assert!(html.contains("approveAction"));
    }

    #[test]
    fn render_shell_with_data() {
        let snapshot = json!({
            "counts": { "running": 2, "retrying": 1 },
            "running": [{
                "issue_identifier": "#42",
                "state": "In Progress",
                "session_id": "thread-1-turn-1",
                "turn_count": 7,
                "last_codex_event": "turn_completed",
                "codex_total_tokens": 2000,
                "codex_input_tokens": 1200,
                "codex_output_tokens": 800,
                "started_at": "2026-03-17T10:00:00Z"
            }],
            "retrying": [{
                "issue_identifier": "#99",
                "attempt": 3,
                "due_at": "2026-03-17T10:05:00Z",
                "error": "no available orchestrator slots"
            }],
            "codex_totals": {
                "input_tokens": 5000, "output_tokens": 2400,
                "total_tokens": 7400, "seconds_running": 1834.2
            },
            "rate_limits": null
        });
        let html = render_shell(&snapshot);
        assert!(html.contains("#42"));
        assert!(html.contains("#99"));
        assert!(html.contains("thread-1-turn-1"));
    }
}
