"""Offline tests: no live game or scanner is contacted."""
import json
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch
import ggscanner_client as client

class ClientTests(unittest.TestCase):
    def test_loopback_only(self):
        for value in ['http://127.0.0.1:18765','http://localhost:1234','http://[::1]:18765']:
            self.assertEqual(client.base_url(value),value)
        for value in ['http://127.0.0.1.evil.test','https://example.com','http://user@localhost','http://localhost/path']:
            with self.assertRaises(ValueError):client.base_url(value)

    def test_unknown_mutation_is_saved_and_resume_does_not_repeat_it(self):
        body={'requestId':'walk-once','key':'w','holdMs':180}
        with tempfile.TemporaryDirectory() as directory:
            with patch.object(client,'transport',side_effect=OSError('connection lost')) as call:
                with self.assertRaisesRegex(RuntimeError,'Outcome unknown'):
                    client.mutate('http://localhost:18765','/input/key',body,directory)
                self.assertEqual(call.call_count,1)
            receipt=Path(directory)/'walk-once.json'
            self.assertEqual(json.loads(receipt.read_text())['request'],body)
            with patch.object(client,'transport',return_value=(404,{'error':'expired'})) as call:
                with self.assertRaisesRegex(RuntimeError,'observe the game'):
                    client.mutate('http://localhost:18765','/input/key',body,directory)
                self.assertEqual(call.call_args.args[1],'/input/result?jobId=walk-once')
                self.assertIsNone(call.call_args.args[2])
            with patch.object(client,'transport') as call:
                with self.assertRaisesRegex(ValueError,'different request'):
                    client.mutate('http://localhost:18765','/input/key',{**body,'holdMs':200},directory)
                call.assert_not_called()

    def test_dry_run_has_no_files_or_network(self):
        with tempfile.TemporaryDirectory() as directory,patch.object(client,'transport') as call:
            result=client.mutate('http://localhost:18765','/input/key',{'requestId':'dry-one','key':'w'},directory,dry_run=True)
            self.assertTrue(result['dryRun']);self.assertEqual(list(Path(directory).iterdir()),[])
            call.assert_not_called()

    def test_input_requires_a_stable_recorded_id(self):
        with tempfile.TemporaryDirectory() as directory,patch.object(client,'transport') as call:
            with self.assertRaisesRegex(ValueError,'explicit requestId'):
                client.mutate('http://localhost:18765','/input/key',{'key':'w'},directory)
            self.assertEqual(list(Path(directory).iterdir()),[]);call.assert_not_called()

    def test_temporary_receipt_file_lock_does_not_repeat_the_post(self):
        original_replace = Path.replace
        replacements = 0
        def sometimes_locked(staged, target):
            nonlocal replacements
            replacements += 1
            # The request ledger succeeds, but the response ledger is briefly locked.
            if replacements in (2, 3):raise PermissionError('file is open')
            return original_replace(staged, target)
        body={'requestId':'one-post','key':'menu','holdMs':80}
        with tempfile.TemporaryDirectory() as directory:
            with patch.object(Path,'replace',autospec=True,side_effect=sometimes_locked), \
                    patch.object(client.time,'sleep') as sleep, \
                    patch.object(client,'transport',return_value=(202,{'jobId':'one-post'})) as call:
                client.mutate('http://localhost:18765','/input/key',body,directory)
            call.assert_called_once()
            self.assertEqual(sleep.call_count,2)
            receipt=Path(directory)/'one-post.json'
            self.assertEqual(json.loads(receipt.read_text())['state'],'response_received')
            self.assertEqual(list(Path(directory).iterdir()),[receipt])

    def test_a_persistent_initial_ledger_lock_prevents_the_post(self):
        with tempfile.TemporaryDirectory() as directory, \
                patch.object(Path,'replace',side_effect=PermissionError('file stays locked')) as replace, \
                patch.object(client.time,'sleep'),patch.object(client,'transport') as call:
            with self.assertRaises(PermissionError):
                client.mutate('http://localhost:18765','/input/key',{'requestId':'blocked','key':'menu'},directory)
            self.assertEqual(replace.call_count,20)
            call.assert_not_called()
            self.assertEqual(list(Path(directory).iterdir()),[])

if __name__=='__main__':unittest.main()
