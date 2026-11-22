'use strict';
let selectedReview = null;
const $ = id => document.getElementById(id);
$('source-id').value = `local-form-${crypto.randomUUID()}`;
function token(){return $('token').value.trim();}
async function request(path, options = {}) {
  if(!token()) throw new Error('Enter your API token first');
  const headers = new Headers(options.headers || {});
  headers.set('Authorization', `Bearer ${token()}`);
  if(options.body) headers.set('Content-Type','application/json');
  const response = await fetch(path,{...options,headers,cache:'no-store'});
  if(!response.ok){
    let detail; try{ detail = (await response.json()).error; }catch{ detail = response.statusText; }
    throw new Error(`${response.status}: ${detail || 'Request failed'}`);
  }
  if(response.headers.get('content-type')?.includes('text/csv')) return await response.blob();
  return await response.json();
}
function report(errorOrText){
  $('result').textContent = errorOrText instanceof Error ? errorOrText.message : String(errorOrText);
}
function show(review){
  selectedReview = review;
  $('detail').textContent = JSON.stringify(review,null,2);
  $('analyze').disabled = review.status !== 'SUBMITTED';
  $('approve').disabled = !['EXTRACTED','NEEDS_REVIEW'].includes(review.status);
  $('stage').disabled = review.status !== 'APPROVED';
  $('audit').disabled = false;
  $('mock').disabled = !['STAGED','MOCK_RECEIVED'].includes(review.status);
}
async function refresh(){
  const reviews = await request('/api/v1/reviews');
  const parent = $('reviews'); parent.replaceChildren();
  if(!reviews.length){parent.textContent='No reviews yet.';return;}
  for(const r of reviews){
    const row=document.createElement('div');row.className='review-row';
    const desc=document.createElement('div');
    const title=document.createElement('b');title.textContent=r.employee_id;
    const subtitle=document.createElement('small');subtitle.textContent=`${r.review_period} · ${r.source_system} · ${r.status}`;
    desc.append(title,subtitle);
    const btn=document.createElement('button');btn.textContent='Inspect';btn.onclick=()=>show(r);
    row.append(desc,btn);parent.append(row);
  }
}
async function act(fn){try{await fn();}catch(e){report(e);}}
$('submit-form').addEventListener('submit',e=>{e.preventDefault();act(async()=>{
  const form=new FormData(e.currentTarget);
  const body=Object.fromEntries(form.entries());body.source_system='form';
  const r=await request('/api/v1/reviews',{method:'POST',body:JSON.stringify(body)});
  report(`Review created: ${r.id} (${r.status})`);
  $('source-id').value=`local-form-${crypto.randomUUID()}`;
  show(r);
  try{await refresh();}catch{ /* employee submitters cannot enumerate reviews */ }
});});
$('refresh').onclick=()=>act(async()=>{await refresh();report('Review queue updated');});
$('analyze').onclick=()=>act(async()=>{show(await request(`/api/v1/reviews/${selectedReview.id}/analyze`,{method:'POST'}));await refresh();report('Extraction finished; inspect evidence before approval');});
$('approve').onclick=()=>act(async()=>{
  if(!confirm('Confirm you reviewed the original and extracted content?'))return;
  show(await request(`/api/v1/reviews/${selectedReview.id}/approve`,{method:'PUT',body:JSON.stringify({revised:null})}));
  await refresh();report('Approved by POC reviewer token');
});
$('stage').onclick=()=>act(async()=>{show(await request(`/api/v1/reviews/${selectedReview.id}/stage`,{method:'POST'}));await refresh();report('Staged in local outbox. Not sent to Workday.');});
$('audit').onclick=()=>act(async()=>{const rows=await request(`/api/v1/reviews/${selectedReview.id}/audit`);report(JSON.stringify(rows,null,2));});
$('export').onclick=()=>act(async()=>{
  const blob=await request('/api/v1/workday/staging.csv');
  const url=URL.createObjectURL(blob);const a=document.createElement('a');a.href=url;a.download='reviewflow-staging.csv';a.click();
  setTimeout(()=>URL.revokeObjectURL(url),3000);report('Staging CSV downloaded. It is not a tenant-validated EIB file.');
});

$('mock').onclick=()=>act(async()=>{
  const receipt=await request(`/api/v1/reviews/${selectedReview.id}/mock-deliver`,{method:'POST'});
  report(JSON.stringify(receipt,null,2)+'\nSIMULATED ONLY — not a Workday receipt');
  show(await request(`/api/v1/reviews/${selectedReview.id}`));await refresh();
});

// Show the provider actually active in the running server, not merely the .env setting.
fetch('/healthz', {cache:'no-store'}).then(r => r.json()).then(h => {
  $('ai-mode').textContent = `AI MODE: ${String(h.ai_mode || 'unknown').toUpperCase()}`;
}).catch(() => { $('ai-mode').textContent = 'AI MODE: unavailable'; });
