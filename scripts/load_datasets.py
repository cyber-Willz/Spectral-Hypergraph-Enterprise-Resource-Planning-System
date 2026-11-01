import csv, json, time, urllib.request, urllib.error, datetime as dt, sys
from concurrent.futures import ThreadPoolExecutor
import os
B=os.environ.get('BASE_URL','http://127.0.0.1:3111'); D=sys.argv[1]  # D = directory holding the unzipped datasets
def call(method,path,body=None):
    req=urllib.request.Request(B+path,method=method,headers={'x-api-key':os.environ.get('API_KEY','k'),'content-type':'application/json'},data=None if body is None else json.dumps(body).encode())
    try:
        with urllib.request.urlopen(req) as r: return r.status,json.loads(r.read() or b'null')
    except urllib.error.HTTPError as e: return e.code,json.loads(e.read() or b'null')
def timed(label,path):
    t=time.time(); s,j=call('GET',path); print(f'  {label:<28}{s}  {1000*(time.time()-t):7.0f} ms'); return j
def par(fn,items,w=8):
    with ThreadPoolExecutor(w) as ex: return list(ex.map(fn,items))
nodes=list(csv.DictReader(open(D+'/Enterprise_Financial_Network_Dataset/enterprise_nodes_2934.csv')))
corp=list(csv.DictReader(open(D+'/Corporate_Financial_Risk_Monitoring_Dataset/enterprise_financial_dataset.csv')))
t0=time.time()
assert call('POST','/api/finance/bank-accounts',{'name':'Main','gl_account_id':1})[0]==201
assert call('POST','/api/finance/cash/transactions',{'bank_account_id':1,'txn_date':'2026-01-01','amount':'50000000000.00','description':'funding','offset_account_id':7})[0]==201
# customers + risk profiles
def mk_customer(n):
    s,j=call('POST','/api/finance/customers',{'name':n['enterprise_id']}); assert s==201,(s,j); return j['id']
ids=par(mk_customer,nodes)
def mk_profile(a):
    cid,n=a
    return call('PUT',f'/api/risk/counterparties/customer/{cid}',{'credit_score':int(n['credit_score']),'debt_ratio':n['debt_ratio'],'liquidity':n['liquidity'],'legal_cases':int(n['legal_cases']),'is_bankrupt':n['bankrupt']=='1'})[0]
st=par(mk_profile,list(zip(ids,nodes))); print('customers',len(ids),'profiles ok',st.count(200),f'{time.time()-t0:.1f}s')
bk=[i for i,n in zip(ids,nodes) if n['bankrupt']=='1']; ok=[i for i,n in zip(ids,nodes) if n['bankrupt']!='1']
# invoices from corporate revenue to non-bankrupt customers
base=dt.date(2026,1,1)
def mk_inv(a):
    k,r=a; d=base+dt.timedelta(days=k%300)
    return call('POST','/api/finance/invoices',{'customer_id':ok[k%len(ok)],'invoice_number':f'INV-{k:05d}','invoice_date':str(d),'amount':f"{float(r['Revenue']):.2f}",'revenue_account_id':8})
res=par(mk_inv,list(enumerate(corp))); inv=[j['id'] for s,j in res if s==201]; print('invoices created',len(inv),'of',len(corp),f'{time.time()-t0:.1f}s')
# invoices to bankrupt customers must be rejected
rej=[call('POST','/api/finance/invoices',{'customer_id':c,'invoice_number':f'BK-{c}','invoice_date':'2026-06-01','amount':'100.00','revenue_account_id':8})[0] for c in bk[:25]]
print('invoices to bankrupt customers (25 tried): status counts',{s:rej.count(s) for s in set(rej)})
# pay half
def pay(i): return call('POST',f'/api/finance/invoices/{i}/receive',{'bank_account_id':1,'payment_date':'2026-09-30'})[0]
ps=par(pay,inv[::2]); print('receipts',ps.count(201),'of',len(ps))
# vendors + bills from operating cost
vend=[call('POST','/api/finance/vendors',{'name':f'Vendor {i}'})[1]['id'] for i in range(50)]
def mk_bill(a):
    k,r=a; d=base+dt.timedelta(days=k%300)
    return call('POST','/api/finance/bills',{'vendor_id':vend[k%50],'bill_number':f'BILL-{k:05d}','bill_date':str(d),'amount':f"{float(r['Operating_Cost']):.2f}",'expense_account_id':12})[0]
bs=par(mk_bill,list(enumerate(corp))); print('bills',bs.count(201),'of',len(bs),f'total load {time.time()-t0:.1f}s')
print('report latency with this volume:')
timed('trial balance','/api/finance/reports/trial-balance?as_of=2026-12-31')
timed('balance sheet','/api/finance/reports/balance-sheet?as_of=2026-12-31')
timed('income statement','/api/finance/reports/income-statement?from=2026-01-01&to=2026-12-31')
timed('ar aging','/api/finance/reports/ar-aging')
timed('ap aging','/api/finance/reports/ap-aging')
timed('cash forecast','/api/finance/cash/forecast?weeks=13')
ex=timed('receivables exposure','/api/risk/receivables-exposure?limit=5')
h=timed('financial health','/api/risk/financial-health?as_of=2026-12-31')
ig=timed('integrity checks','/api/risk/integrity-checks')
timed('risk profile list (high)','/api/risk/counterparties?band=high&limit=500')
print(json.dumps({k:ex[k] for k in ['total_outstanding','total_overdue','assessed_outstanding','unassessed_outstanding','risk_weighted_exposure']}))
print({k:{'customers':v['customers'],'outstanding':v['outstanding']} for k,v in ex['by_band'].items()})
print('health:',json.dumps({'status':h['status'],'metrics':h['metrics'],'score':h['score'],'flags':[f['code'] for f in h['flags']]}))
print('integrity:',ig['overall']); [print('  ',c['status'],c['name'],'-',c['detail'][:150]) for c in ig['checks']]
