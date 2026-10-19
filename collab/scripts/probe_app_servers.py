import concurrent.futures, json, os, queue, subprocess, tempfile, threading, time, sys

CODEX_BIN = os.environ.get('CODEX_COLLAB_BIN', 'codex')
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

class Mock(BaseHTTPRequestHandler):
    def log_message(self, *args): pass
    def do_POST(self):
        import gzip
        raw = self.rfile.read(int(self.headers.get('Content-Length', '0')))
        if self.headers.get('Content-Encoding') == 'gzip': raw = gzip.decompress(raw)
        request = json.loads(raw)
        self.server.calls.append({'path': self.path, 'model': request.get('model'), 'auth_ok': self.headers.get('Authorization') == 'Bearer stage1-fake-' + self.server.label})
        tool_result = any(x.get('type') == 'function_call_output' for x in request.get('input', []) if isinstance(x, dict))
        response_id = 'resp_mock_' + str(len(self.server.calls))
        item = ({'type': 'message', 'id': 'msg_mock', 'role': 'assistant', 'content': [{'type': 'output_text', 'text': self.server.label + ' complete'}]} if tool_result else {'type': 'function_call', 'id': 'fc_mock', 'call_id': 'call_mock', 'name': 'collab_probe', 'arguments': '{"value":"ok"}'})
        events = [{'type': 'response.created', 'response': {'id': response_id}}, {'type': 'response.output_item.added', 'output_index': 0, 'item': dict(item, **({'content': []} if tool_result else {'arguments': ''}))}]
        if tool_result: events.append({'type': 'response.output_text.delta', 'delta': self.server.label + ' complete', 'item_id': 'msg_mock', 'output_index': 0, 'content_index': 0})
        events.extend([{'type': 'response.output_item.done', 'output_index': 0, 'item': item}, {'type': 'response.completed', 'response': {'id': response_id, 'output': [item], 'usage': {'input_tokens': 10, 'output_tokens': 5, 'total_tokens': 15}}}])
        payload = ''.join('event: ' + e['type'] + '\ndata: ' + json.dumps(e) + '\n\n' for e in events).encode()
        self.send_response(200); self.send_header('Content-Type', 'text/event-stream'); self.send_header('Content-Length', str(len(payload))); self.end_headers(); self.wfile.write(payload)

def probe(label):
    with tempfile.TemporaryDirectory(prefix='codex-collab-probe-') as home:
        server = ThreadingHTTPServer(('127.0.0.1', 0), Mock); server.calls = []; server.label = label
        threading.Thread(target=server.serve_forever, daemon=True).start()
        port = server.server_port
        args = [CODEX_BIN, '-c', 'model_provider="collab_probe"', '-c', 'model_providers.collab_probe.name="Local mock"', '-c', f'model_providers.collab_probe.base_url="http://127.0.0.1:{port}/v1"', '-c', 'model_providers.collab_probe.env_key="COLLAB_PROBE_KEY"', '-c', 'model_providers.collab_probe.wire_api="responses"', '-c', 'model_providers.collab_probe.request_max_retries=0', '-c', 'model_providers.collab_probe.stream_max_retries=0', '-c', 'features.codex_hooks=false', 'app-server', '--listen', 'stdio://']
        env = dict(os.environ, CODEX_HOME=home, COLLAB_PROBE_KEY='stage1-fake-' + label)
        for key in list(env):
            if key.startswith(('OPENAI_', 'ANTHROPIC_', 'GEMINI_')): env.pop(key)
        stderr_file = open(Path(home) / 'stderr.log', 'w+')
        p = subprocess.Popen(args, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=stderr_file, text=True, env=env)
        q = queue.Queue()
        def reader():
            for line in p.stdout:
                try: q.put(json.loads(line))
                except ValueError: pass
        threading.Thread(target=reader, daemon=True).start()
        def send(message): p.stdin.write(json.dumps(message) + '\n'); p.stdin.flush()
        def receive(predicate, timeout=30):
            deadline = time.monotonic() + timeout
            while time.monotonic() < deadline:
                try: message = q.get(timeout=min(1, max(.01, deadline-time.monotonic())))
                except queue.Empty:
                    if p.poll() is not None: raise RuntimeError('App Server exited')
                    continue
                if message.get('method') == 'item/tool/call':
                    assert message['params']['tool'] == 'collab_probe'
                    results['tool_calls'] += 1
                    send({'id': message['id'], 'result': {'contentItems': [{'type': 'inputText', 'text': 'probe result'}], 'success': True}})
                if message.get('method') == 'item/agentMessage/delta': results['deltas'].append(message['params']['delta'])
                if predicate(message):
                    if 'error' in message: raise RuntimeError(str(message['error']))
                    return message
            raise TimeoutError('App Server response timeout')
        results = {'label': label, 'port': port, 'tool_calls': 0, 'deltas': []}
        try:
            send({'id': 1, 'method': 'initialize', 'params': {'clientInfo': {'name': 'collab_probe', 'version': '0.1.0'}, 'capabilities': {'experimentalApi': True}}})
            receive(lambda m: m.get('id') == 1); send({'method': 'initialized'})
            send({'id': 2, 'method': 'thread/start', 'params': {'model': 'mock-' + label, 'modelProvider': 'collab_probe', 'cwd': home, 'ephemeral': True, 'approvalPolicy': 'never', 'sandbox': 'read-only', 'dynamicTools': [{'type': 'function', 'name': 'collab_probe', 'description': 'Local protocol probe', 'inputSchema': {'type': 'object', 'properties': {'value': {'type': 'string'}}, 'required': ['value'], 'additionalProperties': False}}]}})
            thread = receive(lambda m: m.get('id') == 2)['result']['thread']['id']
            send({'id': 3, 'method': 'turn/start', 'params': {'threadId': thread, 'input': [{'type': 'text', 'text': 'Call collab_probe then return completion.', 'text_elements': []}]}})
            receive(lambda m: m.get('id') == 3)
            completed = receive(lambda m: m.get('method') == 'turn/completed', timeout=45)
            results['status'] = completed['params']['turn']['status']; results['requests'] = server.calls
            assert results['status'] == 'completed', completed
            assert results['tool_calls'] == 1, results
            assert ''.join(results['deltas']) == label + ' complete', results
            assert len(server.calls) == 2 and all(c['model'] == 'mock-' + label and c['auth_ok'] for c in server.calls), results
            return results
        except Exception:
            stderr_file.flush(); stderr_file.seek(0); print(stderr_file.read()[-3000:]); raise
        finally:
            p.terminate()
            try: p.wait(timeout=5)
            except subprocess.TimeoutExpired: p.kill(); p.wait()
            server.shutdown(); server.server_close(); stderr_file.close()

if __name__ == '__main__':
    with concurrent.futures.ThreadPoolExecutor(max_workers=2) as pool:
        results = list(pool.map(probe, ['alpha', 'beta']))
    report = {'runtime': subprocess.check_output([CODEX_BIN, '--version'], text=True).strip(), 'results': results}
    encoded = json.dumps(report, indent=2)
    if len(sys.argv) > 1: Path(sys.argv[1]).write_text(encoded + '\n')
    print(encoded)
