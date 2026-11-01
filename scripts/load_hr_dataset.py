"""Loads the salary dataset through the API as employees (+ performance reviews) and prints the
compensation benchmark. usage: load_hr_dataset.py <dir with employee_salary_regression.csv>"""
import csv, json, os, sys, time, datetime as dt, urllib.request, urllib.error
from concurrent.futures import ThreadPoolExecutor
B = os.environ.get('BASE_URL', 'http://127.0.0.1:3111'); K = os.environ.get('API_KEY', 'k')
def call(method, path, body=None):
    req = urllib.request.Request(B + path, method=method, headers={'x-api-key': K, 'content-type': 'application/json'},
                                 data=None if body is None else json.dumps(body).encode())
    try:
        with urllib.request.urlopen(req) as r: return r.status, json.loads(r.read() or b'null')
    except urllib.error.HTTPError as e: return e.code, json.loads(e.read() or b'null')
rows = list(csv.DictReader(open(os.path.join(sys.argv[1], 'employee_salary_regression.csv'))))
today = dt.date.today().isoformat(); t0 = time.time()
def mk(a):
    i, r = a
    s, j = call('POST', '/api/hr/employees', {
        'employee_number': r['employee_id'], 'first_name': 'E', 'last_name': r['employee_id'],
        'email': f"{r['employee_id'].lower()}@example.com", 'job_title': r['job_role'], 'hire_date': today,
        'base_salary': f"{float(r['annual_salary_usd']):.2f}",
        'education_level': r['education_level'].lower().replace(' ', '_'),
        'years_experience': int(r['years_experience']), 'city_tier': int(r['city_tier']),
        'remote_work': r['remote_work'] == '1'})
    assert s == 201, (s, j); return j['id']
with ThreadPoolExecutor(8) as ex: ids = list(ex.map(mk, list(enumerate(rows))))
print(f'employees loaded: {len(ids)} in {time.time()-t0:.1f}s')
def review(a):
    i, r = a
    return call('POST', '/api/hr/performance/reviews', {'employee_id': ids[i], 'reviewer_id': ids[(i + 1) % len(ids)],
        'period_label': '2026', 'period_start': '2026-01-01', 'period_end': '2026-12-31',
        'rating': max(1, min(5, round(float(r['performance_score']))))})[0]
with ThreadPoolExecutor(8) as ex: rs = list(ex.map(review, list(enumerate(rows))[:600]))
print('reviews created:', rs.count(201))
