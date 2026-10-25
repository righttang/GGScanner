#!/usr/bin/env python3
"""GGScanner loopback client. Record before POST; never retry an unknown mutation."""
import argparse
import datetime
import json
import platform
import re
import subprocess
import sys
import urllib.error
import urllib.parse
import urllib.request
import uuid
from pathlib import Path

GET = {'health':'/health','status':'/status','result':'/result','input-result':'/input/result',
       'characters':'/characters','weapons':'/weapons','artifacts':'/artifacts'}
POST = {'scan':'/scan','equip':'/equip','manage':'/manage','click':'/input/click',
        'key':'/input/key','stop':'/stop'}

def base_url(value):
    url = urllib.parse.urlsplit(value)
    if (url.scheme != 'http' or url.hostname not in {'127.0.0.1','localhost','::1'}
            or url.username or url.password or url.path not in {'','/'} or url.query or url.fragment):
        raise ValueError('Use an HTTP loopback base URL without a path or credentials')
    _ = url.port  # Validate its range too.
    return value.rstrip('/')

def save(path, value):
    path.parent.mkdir(parents=True, exist_ok=True)
    staged = path.with_suffix('.tmp')
    staged.write_text(json.dumps(value, ensure_ascii=False, indent=2), encoding='utf-8')
    staged.replace(path)

def transport(base, endpoint, body, mode='auto'):
    base = base_url(base)
    if mode == 'auto':
        mode = 'windows-curl' if 'microsoft' in platform.release().lower() else 'native'
    data = json.dumps(body, ensure_ascii=False).encode('utf-8') if body is not None else None
    if mode == 'windows-curl':
        cmd = ['curl.exe','--noproxy','*','--silent','--show-error','--max-time','20',
               '--write-out','\n%{http_code}']
        if data is not None:cmd += ['--header','Content-Type: application/json','--data-binary','@-']
        response = subprocess.run(cmd + [base+endpoint], input=data, capture_output=True, timeout=25)
        if response.returncode:raise OSError(response.stderr.decode(errors='replace'))
        raw, status = response.stdout.rsplit(b'\n',1)
        return int(status), json.loads(raw.decode('utf-8-sig'))
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
    req = urllib.request.Request(base+endpoint, data=data, headers={'Content-Type':'application/json'})
    try:
        with opener.open(req, timeout=20) as response:
            return response.status, json.load(response)
    except urllib.error.HTTPError as error:
        return error.code, json.load(error)

def mutate(base, endpoint, body, ledger, mode='auto', dry_run=False):
    if endpoint not in POST.values():raise ValueError('Unsupported mutation endpoint')
    body = dict(body)
    is_input = endpoint.startswith('/input/')
    if is_input and not body.get('requestId'):
        raise ValueError('Input requires an explicit requestId saved in your JSON file')
    request_id = body['requestId'] if is_input else uuid.uuid4().hex
    if not isinstance(request_id,str) or not re.fullmatch(r'[A-Za-z0-9_-]{1,128}',request_id):
        raise ValueError('requestId must contain 1–128 letters, digits, hyphens or underscores')
    if dry_run:return {'dryRun':True,'endpoint':endpoint,'body':body}
    receipt = Path(ledger)/(request_id+'.json')
    if receipt.exists():
        previous = json.loads(receipt.read_text(encoding='utf-8'))
        if previous.get('baseUrl') != base or previous.get('endpoint') != endpoint or previous.get('request') != body:
            raise ValueError('Recorded ID belongs to a different request')
        if not is_input:raise ValueError('This operation is already recorded; inspect its result')
        # A restarted service may return 404. Never re-POST an expired receipt.
        status, result = transport(base,'/input/result?jobId='+request_id,None,mode)
        previous.update(httpStatus=status,result=result,resumedByReadOnlyQuery=True)
        save(receipt, previous)
        if status >= 400:raise RuntimeError(f'HTTP {status}; observe the game before continuing: {receipt}')
        return result
    record = {'createdAt':datetime.datetime.now(datetime.timezone.utc).isoformat(),
              'baseUrl':base,'endpoint':endpoint,'request':body,'state':'submitted_outcome_unknown'}
    save(receipt,record)
    try:
        status, result = transport(base,endpoint,body,mode)
    except Exception as error:
        record['transportError']=str(error);save(receipt,record)
        raise RuntimeError(f'Outcome unknown; do not resend. Inspect {receipt}') from error
    record.update(httpStatus=status,result=result,state='response_received');save(receipt,record)
    if status >= 400:raise RuntimeError(f'HTTP {status}; inspect {receipt}')
    return result

def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__,epilog='Example: ggscanner_client.py key walk.json; ggscanner_client.py input-result <requestId>')
    parser.add_argument('--base-url',default='http://127.0.0.1:18765')
    parser.add_argument('--transport',choices=['auto','native','windows-curl'],default='auto')
    parser.add_argument('--ledger',type=Path,default=Path('ggscanner-actions'))
    parser.add_argument('--dry-run',action='store_true')
    parser.add_argument('action',choices=sorted(GET.keys()|POST.keys()))
    parser.add_argument('argument',nargs='?',help='POST JSON file, or jobId for result/data/stop')
    args=parser.parse_args(argv);base=base_url(args.base_url)
    if args.action in GET:
        endpoint=GET[args.action]
        if args.argument:endpoint+='?'+urllib.parse.urlencode({'jobId':args.argument})
        elif args.action not in {'health','status','artifacts'}:parser.error('jobId required')
        if args.dry_run:result={'dryRun':True,'endpoint':endpoint}
        else:
            status,result=transport(base,endpoint,None,args.transport)
            if status>=400:raise RuntimeError(f'HTTP {status}: {json.dumps(result,ensure_ascii=False)}')
    else:
        if not args.argument:parser.error('JSON file or stop jobId required')
        body={'jobId':args.argument} if args.action=='stop' else json.loads(Path(args.argument).read_text(encoding='utf-8-sig'))
        result=mutate(base,POST[args.action],body,args.ledger,args.transport,args.dry_run)
    print(json.dumps(result,ensure_ascii=False,indent=2))

if __name__=='__main__':
    try:main()
    except Exception as error:
        print(str(error),file=sys.stderr);sys.exit(1)
