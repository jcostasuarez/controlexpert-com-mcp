"""Exercise the real RMCP stdio transport; optional read-only broker smoke test.

python scripts/smoke_mcp.py [--exe target/release/controlexpert-com-mcp.exe] [--live]
--live creates only the UDE broker and reads InterfaceVersion. It never opens
an application/project, writes to a project, connects to a PLC, or downloads.
--live --session also starts an empty headless application and checks that
policy-blocked operations are rejected before reaching COM.
"""
import argparse
import json
import os
from pathlib import Path
import queue
import subprocess
import sys
import threading
import time


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--exe', type=Path, default=Path(__file__).resolve().parents[1] / 'target/debug/controlexpert-com-mcp.exe')
    parser.add_argument('--live', action='store_true')
    parser.add_argument('--session', action='store_true', help='Also starts an application without opening or modifying projects; requires --live')
    parser.add_argument('--viewer', action='store_true', help='With --live --session, opens the read-only viewer of the empty session on request')
    parser.add_argument('--protocol', default='2025-11-25')
    parser.add_argument('--request-timeout', type=float, default=300, help='Client wait; does not cancel COM')
    parser.add_argument('--shutdown-timeout', type=float, default=300, help='Wait for the orderly shutdown; the process is not killed when it expires')
    parser.add_argument('--eof-cleanup', action='store_true', help='With --live, keeps a broker reference to verify that closing stdin releases it')
    args = parser.parse_args()
    if args.session and not args.live:
        parser.error('--session requires --live')
    if args.viewer and not (args.live and args.session):
        parser.error('--viewer requires --live --session')
    if args.eof_cleanup and not args.live:
        parser.error('--eof-cleanup requires --live')
    if args.request_timeout <= 0 or args.shutdown_timeout <= 0:
        parser.error('Timeouts must be positive')
    locked = ['CE_MCP_ALLOW_BUILD', 'CE_MCP_ALLOW_PLC_CONNECTION', 'CE_MCP_ALLOW_PLC_WRITE', 'CE_MCP_ALLOW_SECURITY_ADMIN', 'CE_MCP_ALLOW_PRIVATE_APIS']
    policy_env = dict(os.environ, **{name: '0' for name in locked})
    process = subprocess.Popen([str(args.exe.resolve()), '--ce-version', '15.1'], env=policy_env, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                               stderr=subprocess.PIPE, text=True, encoding='utf-8')
    messages = queue.Queue()
    stderr = []

    def read_stdout():
        for line in process.stdout:
            try:
                messages.put(json.loads(line))
            except ValueError:
                messages.put(RuntimeError('Non-JSON stdout: ' + line))
        messages.put(RuntimeError('Server closed stdout'))

    stdout_reader = threading.Thread(target=read_stdout, daemon=True)
    stderr_reader = threading.Thread(target=lambda: stderr.extend(process.stderr), daemon=True)
    stdout_reader.start()
    stderr_reader.start()
    request_id = 0
    checks = []
    buffered = {}
    ready = False
    reset_done = False

    def begin(method, params, notification=False):
        nonlocal request_id
        request_id += 1
        msg = {'jsonrpc': '2.0', 'method': method, 'params': params}
        if not notification:
            msg['id'] = request_id
        process.stdin.write(json.dumps(msg) + '\n')
        process.stdin.flush()
        if notification:
            return None
        return request_id

    def receive(ident):
        deadline = time.monotonic() + args.request_timeout
        while True:
            response = buffered.pop(ident, None)
            if response is None:
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    raise TimeoutError('The MCP response is still pending; this does not cancel the COM call.')
                response = messages.get(timeout=remaining)
            if isinstance(response, Exception):
                raise response
            if response.get('id') == ident:
                assert 'error' not in response, response
                return response['result']
            if 'id' in response:
                buffered[response['id']] = response

    def send(method, params, notification=False):
        ident = begin(method, params, notification)
        return None if notification else receive(ident)

    def call(name, arguments=None, error=False):
        response = send('tools/call', {'name': name, 'arguments': arguments or {}})
        assert bool(response.get('isError')) == error, response
        if error:
            return response
        value = response.get('structuredContent')
        assert value is not None, response
        return value

    try:
        initialized = send('initialize', {'protocolVersion': args.protocol, 'capabilities': {}, 'clientInfo': {'name': 'controlexpert-smoke', 'version': '1.0'}})
        assert initialized['serverInfo']['name'] == 'controlexpert-com-mcp'
        send('notifications/initialized', {}, notification=True)
        ready = True
        tools = send('tools/list', {})['tools']
        names = {tool['name'] for tool in tools}
        assert len(names) == 18 and {'ce_screens_export', 'ce_screens_import', 'ce_session_start', 'ce_viewer_open', 'ce_api_call', 'ce_catalog_list', 'ce_probe', 'ce_runtime_status'} <= names, names
        info = call('ce_info')
        assert info['sdk'] == 'RMCP 3.5.1' and info['selected_version'] == '15.1', info
        assert info['catalog']['functions_and_accessors'] == 1277
        assert 'Schneider' in info['disclaimer']
        for capability in ['allow_build', 'allow_plc_connection', 'allow_plc_write', 'allow_security_admin', 'allow_private_apis']:
            assert info['execution_policy'][capability] is False, info['execution_policy']
        assert info['execution_policy']['headless_start_required'] is True
        checks.append('initialize, discovery and RMCP 3.5.1 metadata')
        status = call('ce_runtime_status')
        assert status['accepting_requests'] and status['worker_running']
        assert status['queued_operations'] == 0 and status['active_operation'] is None, status
        assert info['lifecycle']['graceful_sta_shutdown'] is True
        assert info['lifecycle']['application_gate'] is True
        assert status['application_ready'] is False
        checks.append('runtime status without COM roundtrip and lifecycle metadata')
        rejected=subprocess.run([str(args.exe.resolve()),'--ce-version','15.2'],capture_output=True,text=True,encoding='utf-8',timeout=5)
        assert rejected.returncode != 0 and not rejected.stdout and 'unsupported_version' in rejected.stderr
        checks.append('unsupported Control Expert versions fail before serving MCP')
        first = call('ce_api_search', {'query': 'BuildAll', 'interface': 'UnityProServer.IProject', 'limit': 1})
        assert first['total'] > 0 and len(first['items']) == 1
        described = call('ce_api_describe', {'id': first['items'][0]['id']})
        assert described['contract']['io']['return']['type'] == 'void'
        assert described['execution_policy']['restriction'] == 'build'
        assert described['execution_policy']['enabled'] is False
        analyze = call('ce_api_describe', {'id': 'UnityProServer.IProject.Analyze:method'})
        assert analyze['execution_policy']['restriction'] is None
        expected = {'UnityProServer.IApplication.TargetConnectionOpen:method': 'plc_connection',
                    'UnityProServer.IApplication.TargetSendCommand:method': 'plc_write',
                    'UnitySecurityServer.IUBServ.UBSDeleteUser:method': 'security_admin',
                    'APPLICATIONFORUNITTESTLib.IUnitTest.RunTest:method': 'private_api',
                    'PSExeLib.IHealthServerGenMulti.KillProcess:method': 'process_termination'}
        for api_id, restriction in expected.items():
            assert call('ce_api_describe', {'id': api_id})['execution_policy']['restriction'] == restriction, api_id
        checks.append('execution policy published per contract')
        assert call('ce_catalog_list', {'section': 'libraries', 'limit': 100})['total'] == 17
        call('ce_catalog_list', {'section': 'bibliotecas'}, error=True)
        assert len(call('ce_enum_describe', {'id': 'UnityProServer.epsrLanguage'})['values']) > 0
        assert call('ce_interface_describe', {'id': 'PSRDtmLib.IProject3'})['name'] == 'IProject3'
        checks.append('catalog search, pagination, contracts, interfaces and enums')
        call('ce_api_call', {'handle': 'invalid', 'api_id': 'UnityProServer.IVariables.Add:method'}, error=True)
        call('ce_api_describe', {'id': 'not-an-api'}, error=True)
        call('ce_api_search', {'limit': 101}, error=True)
        call('ce_api_search', {'unknown_field': True}, error=True)
        call('ce_api_call', {'handle': 'invalid', 'api_id': 'UnityProServer.IProject.BuildAll:method'}, error=True)
        if sys.platform == 'win32':
            blocked = call('ce_api_call', {'handle': 'invalid', 'api_id': 'UnityProServer.IApplication.Version:get'}, error=True)
            assert blocked['structuredContent']['error']['code'] == 'application_not_ready', blocked
            blocked = call('ce_object_create', {'class_id': 'UnityProServer.Application'}, error=True)
            assert blocked['structuredContent']['error']['code'] == 'application_not_ready', blocked
            blocked = call('ce_object_query', {'handle': 'invalid', 'interface': 'UnityProServer.IApplication'}, error=True)
            assert blocked['structuredContent']['error']['code'] == 'application_not_ready', blocked
            checks.append('business calls, object creation and queries blocked before application startup')
        call('ce_api_call', {'handle': 'invalid', 'api_id': 'UnityProServer.DIApplicationEvents.AfterSave:method',
                             'arguments': {'VarSignature': 0}}, error=True)
        assert call('ce_objects')['objects'] == []
        checks.append('in-band errors, unknown fields, required arguments, events and invalid handles')
        live_result = None
        if args.live:
            probe = call('ce_probe')
            assert probe['compatible'] is True, probe
            assert probe['application_activation_tested'] is False and probe['compatibility_scope'], probe
            broker = call('ce_object_create', {'class_id': 'UnityProBroker.PServerBroker'})
            handle = broker['$object']
            try:
                version = call('ce_api_call', {'handle': handle, 'api_id': 'UnityProBroker.IPServerBroker.InterfaceVersion:get'})
                assert isinstance(version['return'], int), version
                live_result = {'installation': probe['installed_version'], 'broker_interface_version': version['return']}
            finally:
                call('ce_object_release', {'handle': handle})
            assert call('ce_runtime_status')['application_ready'] is False
            assert call('ce_objects')['objects'] == []
            checks.append('installed 15.1 version, live UDE broker activation, property read and release')
            if args.session:
                opening = begin('tools/call', {'name': 'ce_session_start', 'arguments': {}})
                live_result['opening_status'] = call('ce_runtime_status')
                opened = receive(opening)
                assert not opened.get('isError'), opened
                session=opened['structuredContent']
                assert session['headless'] is True
                assert call('ce_runtime_status')['application_ready'] is True
                for interface in ['IProject', 'IProject2']:
                    for method in ['Build', 'BuildAll']:
                        # Deliberately use the application handle: policy must
                        # reject before QueryInterface, without any real build.
                        blocked = call('ce_api_call', {'handle': session['application']['$object'], 'api_id': f'UnityProServer.{interface}.{method}:method'}, error=True)
                        assert blocked['structuredContent']['error']['code'] == 'operation_blocked', blocked
                app = session['application']['$object']
                # Rejected before COM: no connection is attempted and no object is created.
                blocked = call('ce_api_call', {'handle': app, 'api_id': 'UnityProServer.IApplication.TargetConnectionOpen:method',
                                               'arguments': {'target': 'psrConnectToPlc', 'BstrNameReservation': 'smoke', 'ConnectionMode': 'psrConnectionModePrimary', 'pProject': None}}, error=True)
                assert blocked['structuredContent']['error']['details']['reason'] == 'plc_connection_disabled', blocked
                blocked = call('ce_object_query', {'handle': app, 'interface': 'UnitySecurityServer.ISecurity'}, error=True)
                assert blocked['structuredContent']['error']['details']['reason'] == 'security_admin_disabled', blocked
                blocked = call('ce_object_create', {'class_id': 'UnitySecurityServer.Security'}, error=True)
                assert blocked['structuredContent']['error']['code'] == 'operation_blocked', blocked
                checks.append('PLC connections, security queries and restricted coclasses blocked before COM')
                assert set(session['timings_ms']) == {'broker_activation', 'application_open_or_create', 'total'}, session
                assert all(isinstance(value, int) and value >= 0 for value in session['timings_ms'].values()), session
                live_result['timings_ms']=session['timings_ms']
                app=session['application']['$object']
                version=call('ce_api_call',{'handle':app,'api_id':'UnityProServer.IApplication.Version:get'})
                project_open=call('ce_api_call',{'handle':app,'api_id':'UnityProServer.IApplication.IsProjectOpen:get'})
                live_result['application_version']=version['return']
                live_result['project_open']=project_open['return']
                assert call('ce_api_call', {'handle': app, 'api_id': 'UnityProServer.IApplication.IsVisible:get'})['return'] == 0
                for mode in [1, 'psrHmiReadWrite']:
                    blocked = call('ce_api_call', {'handle': app, 'api_id': 'UnityProServer.IApplication.DisplayStart:method', 'arguments': {'OptionReadWrite': mode, 'comandLine': ''}}, error=True)
                    assert blocked['structuredContent']['error']['details']['reason'] == 'read_only_viewer_required', blocked
                if args.viewer:
                    viewer = call('ce_viewer_open', {'handle': app})
                    assert viewer['viewer_mode'] == 'read_only' and viewer['same_session'] is True, viewer
                    assert viewer['application']['$object'] == app, viewer
                    assert call('ce_api_call', {'handle': app, 'api_id': 'UnityProServer.IApplication.IsVisible:get'})['return'] != 0
                    assert call('ce_api_call', {'handle': app, 'api_id': 'UnityProServer.IApplication.Version:get'})['return'] == version['return']
                    live_result['viewer'] = viewer
                    checks.append('explicit read-only viewer on same application, editable viewers blocked, COM still responsive')
                call('ce_object_release',{'handle':app})
                call('ce_object_release',{'handle':session['broker']['$object']})
                assert call('ce_runtime_status')['application_ready'] is False
                assert call('ce_objects')['application_ready'] is False
                checks.append('application session creation, version property, project state read and reference release')
        if args.eof_cleanup:
            call('ce_object_create', {'class_id': 'UnityProBroker.PServerBroker'})
        else:
            assert call('ce_reset')['released_objects'] == 0
            reset_done = True
    finally:
        failing = sys.exc_info()[0] is not None
        if ready and not reset_done and (failing or not args.eof_cleanup) and process.poll() is None:
            try:
                call('ce_reset')
            except Exception as exc:
                stderr.append(f'Could not confirm ce_reset: {exc}\n')
        process.stdin.close()
        try:
            process.wait(timeout=args.shutdown_timeout)
        except subprocess.TimeoutExpired as exc:
            raise RuntimeError(f'Shutdown is still pending (PID {process.pid}). The process is not killed: COM may still be running.\n'+''.join(stderr)) from exc
        stdout_reader.join(timeout=1)
        stderr_reader.join(timeout=1)
        if process.returncode:
            raise RuntimeError(f'Server exit {process.returncode}: ' + ''.join(stderr))
    events=[]
    for line in stderr:
        try: events.append(json.loads(line))
        except ValueError: pass
    stopped=next(event for event in events if event.get('event') == 'com_worker_stopped')
    assert stopped['details']['references_released'] is True, events
    assert any(event.get('event') == 'runtime_shutdown_finished' for event in events), events
    if args.eof_cleanup:
        assert stopped['details']['released_objects'] == 1, stopped
        checks.append('EOF releases an intentionally retained live COM reference')
    checks.append('EOF joins COM worker and confirms ordered shutdown on stderr')
    print(json.dumps({'ok': True, 'protocol': initialized['protocolVersion'], 'tools': len(tools), 'dispatch_operations': info['dispatch_operations'],
                      'checks': checks, 'live': live_result}, ensure_ascii=False, indent=2))


if __name__ == '__main__':
    main()
