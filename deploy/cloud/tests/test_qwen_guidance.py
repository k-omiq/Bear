"""Request-local Qwen prompts and cross-language request identity, no GPU."""
import dataclasses
import json
from pathlib import Path
import unittest
from deploy.cloud.common.contract import GUIDED_RECIPE_IDS, SIZED_RECIPE_IDS, WORK_LONG_SIDES, ContractValidationError, Flux3Generation, JobRequestMetadata, QwenEdit, RenderRecipe, compute_request_digest, validate_job_request_metadata
from deploy.cloud.common.manifest import RECIPE_PROD_FLUX3, RECIPE_PROD_FLUX3_BF16, RECIPE_PROD_QWEN, WORK_LONG_SIDE, RECIPE_PROD_SDNQ, RECIPE_PROD_SDNQ_9B, get_pinned_recipe
from deploy.cloud.common.qwen import build_prompt, QwenEditRunner
from deploy.cloud.tests.test_qwen import qwen_modules

FIXTURE = Path(__file__).resolve().parents[1] / 'fixtures/qwen_guidance_request.json'
SIZED_FIXTURE = FIXTURE.parent / 'flux3_work_size_request.json'
GENERATION_FIXTURE = FIXTURE.parent / 'flux3_generation_request.json'

class GuidanceTest(unittest.TestCase):
    def test_blank_description_has_useful_scoped_instructions(self):
        prompt = build_prompt(QwenEdit(target='sound_effect'))
        self.assertIn('sound-effect lettering', prompt)
        self.assertIn('image edges', prompt)
        self.assertIn('Preserve dialogue', prompt)
        self.assertIn('hands, faces, hair', prompt)
        self.assertIn('Add no new text', prompt)

    def test_description_supplements_preservation_and_never_leaks_to_next_request(self):
        modules = qwen_modules(image_size=(64, 64))
        runner = QwenEditRunner('/weights', modules=modules)
        for edit in [QwenEdit('sound_effect', 'Big black letters beside the hand'), QwenEdit('dialogue', '')]:
            runner.render_png(b'png', b'', 64, 64, 1, 8, 100, qwen_edit=edit)
        first, second = [call['prompt'] for call in modules.pipeline.calls]
        self.assertIn('Big black letters beside the hand', first)
        self.assertIn('Preserve all character artwork', first)
        self.assertNotIn('Big black letters', second)
        self.assertIn('dialogue lettering', second)
        self.assertEqual(modules.pipeline.active, (['lightning', 'fukidashi'], [1.0, 1.0]))

    def test_hint_location_focuses_the_prompt_without_sending_a_mask_to_qwen(self):
        import numpy as np
        mask = np.zeros((300, 300), bool)
        mask[5:40, 250:295] = True
        prompt = build_prompt(QwenEdit('sound_effect', ''), mask, np)
        self.assertIn('upper right area', prompt)
        self.assertIn('text outside the target area', prompt)

    def test_limits_and_recipe_gate(self):
        recipe = get_pinned_recipe(RECIPE_PROD_QWEN).to_dict()
        for description in ['', 'é字 ' * 166, 'x' * 500]:
            parsed = RenderRecipe.from_dict({**recipe, 'qwen_edit': {'target': 'other', 'description': description}})
            self.assertEqual(parsed.qwen_edit.description, description)
        for edit in [{'target': 'draw_art', 'description': ''}, {'target': 'other', 'description': 'x' * 501}, {'target': 'other', 'description': 'a\nb'}, {'target': 'other', 'description': False}]:
            with self.assertRaises(ContractValidationError):
                RenderRecipe.from_dict({**recipe, 'qwen_edit': edit})
        with self.assertRaises(ContractValidationError):
            RenderRecipe.from_dict({**recipe, 'recipe_id': 'mc-qwen-image-edit-2511-v3', 'qwen_edit': {'target': 'dialogue', 'description': ''}})

    def test_klein_v2_takes_guidance_and_v1_does_not(self):
        edit = {'target': 'sound_effect', 'description': 'a brick wall'}
        for recipe_id in (RECIPE_PROD_SDNQ, RECIPE_PROD_SDNQ_9B):
            recipe = get_pinned_recipe(recipe_id).to_dict()
            self.assertEqual(RenderRecipe.from_dict({**recipe, 'qwen_edit': edit}).qwen_edit, QwenEdit(**edit))
            with self.assertRaises(ContractValidationError):
                RenderRecipe.from_dict({**recipe, 'recipe_id': recipe_id.replace('-v2', '-v1'), 'qwen_edit': edit})

    def test_every_guided_recipe_is_pinned(self):
        self.assertEqual(GUIDED_RECIPE_IDS, {RECIPE_PROD_QWEN, RECIPE_PROD_SDNQ, RECIPE_PROD_SDNQ_9B, RECIPE_PROD_FLUX3, RECIPE_PROD_FLUX3_BF16})
        for recipe_id in GUIDED_RECIPE_IDS:
            self.assertIsNotNone(get_pinned_recipe(recipe_id), recipe_id)

    def test_validated_job_forwards_guidance_to_the_runner(self):
        from deploy.cloud.common.flux import render_job
        from deploy.cloud.common.contract import provisional_fixture_limits
        meta = JobRequestMetadata.from_dict(json.loads(FIXTURE.read_text()))
        image = (FIXTURE.parent / 'tiny_image.png').read_bytes()
        hint = (FIXTURE.parent / 'tiny_hint.png').read_bytes()
        class RecordingRunner:
            def render_png(inner, *args, **kwargs):
                inner.args = args
                inner.kwargs = kwargs
                return image
        runner = RecordingRunner()
        render_job(runner, meta, image, hint, provisional_fixture_limits())
        self.assertEqual(runner.args[4:7], (1, 8, 100))
        self.assertEqual(runner.kwargs['qwen_edit'], meta.recipe.qwen_edit)

    def test_shared_digest_and_edit_identity(self):
        meta = JobRequestMetadata.from_dict(json.loads(FIXTURE.read_text()))
        self.assertEqual(meta.request_digest, compute_request_digest(meta))
        self.assertEqual(JobRequestMetadata.from_dict(meta.to_dict()), meta)
        for edit in [QwenEdit('dialogue', meta.recipe.qwen_edit.description), QwenEdit('sound_effect', '')]:
            changed = dataclasses.replace(meta, recipe=dataclasses.replace(meta.recipe, qwen_edit=edit))
            self.assertNotEqual(compute_request_digest(changed), meta.request_digest)

    def test_working_size_is_flux3_only_on_the_list_and_in_the_digest(self):
        self.assertEqual(SIZED_RECIPE_IDS, {RECIPE_PROD_FLUX3, RECIPE_PROD_FLUX3_BF16})
        self.assertEqual(WORK_LONG_SIDES[0], WORK_LONG_SIDE)
        meta = JobRequestMetadata.from_dict(json.loads(SIZED_FIXTURE.read_text()))
        self.assertEqual(meta.recipe.work_long_side, 1280)
        self.assertEqual(meta.request_digest, compute_request_digest(meta))
        self.assertEqual(JobRequestMetadata.from_dict(meta.to_dict()), meta)
        for side in (None, 1024):
            changed = dataclasses.replace(meta, recipe=dataclasses.replace(meta.recipe, work_long_side=side))
            self.assertNotEqual(compute_request_digest(changed), meta.request_digest)
        recipe = get_pinned_recipe(RECIPE_PROD_FLUX3).to_dict()
        for side in WORK_LONG_SIDES:
            self.assertEqual(RenderRecipe.from_dict({**recipe, 'work_long_side': side}).work_long_side, side)
        for side in (1100, 2048, True, '1280'):
            with self.assertRaises(ContractValidationError):
                RenderRecipe.from_dict({**recipe, 'work_long_side': side})
        with self.assertRaises(ContractValidationError):
            RenderRecipe.from_dict({**get_pinned_recipe(RECIPE_PROD_SDNQ).to_dict(), 'work_long_side': 1280})

    def test_validated_job_forwards_the_working_size_to_the_runner(self):
        from deploy.cloud.common.flux import render_job
        from deploy.cloud.common.contract import provisional_fixture_limits
        meta = JobRequestMetadata.from_dict(json.loads(SIZED_FIXTURE.read_text()))
        image = (FIXTURE.parent / 'tiny_image.png').read_bytes()
        hint = (FIXTURE.parent / 'tiny_hint.png').read_bytes()
        class RecordingRunner:
            def render_png(inner, *args, **kwargs):
                inner.kwargs = kwargs
                return image
        runner = RecordingRunner()
        render_job(runner, meta, image, hint, provisional_fixture_limits())
        self.assertEqual(runner.kwargs['work_long_side'], 1280)
        unsized = dataclasses.replace(meta, recipe=dataclasses.replace(meta.recipe, work_long_side=None))
        unsized = dataclasses.replace(unsized, request_digest=compute_request_digest(unsized))
        render_job(runner, unsized, image, hint, provisional_fixture_limits())
        self.assertNotIn('work_long_side', runner.kwargs)

    def test_a_generation_choice_is_flux3_only_never_empty_and_in_the_digest(self):
        meta = JobRequestMetadata.from_dict(json.loads(GENERATION_FIXTURE.read_text()))
        self.assertEqual(meta.recipe.flux3_generation, Flux3Generation(reference_prompt=True, marker=True, megapixel_size=True))
        self.assertEqual(meta.request_digest, compute_request_digest(meta))
        self.assertEqual(JobRequestMetadata.from_dict(meta.to_dict()), meta)
        for other in (None, Flux3Generation(reference_prompt=True, marker=True, reference_shift=True, megapixel_size=True)):
            changed = dataclasses.replace(meta, recipe=dataclasses.replace(meta.recipe, flux3_generation=other))
            self.assertNotEqual(compute_request_digest(changed), meta.request_digest)
        recipe = get_pinned_recipe(RECIPE_PROD_FLUX3).to_dict()
        for bad in ({}, {'reference_prompt': False}, {'marker': 1}, {'colour': True}, [True]):
            with self.assertRaises(ContractValidationError):
                RenderRecipe.from_dict({**recipe, 'flux3_generation': bad})
        with self.assertRaises(ContractValidationError):
            RenderRecipe.from_dict({**recipe, 'work_long_side': 1280, 'flux3_generation': {'megapixel_size': True}})
        self.assertEqual(RenderRecipe.from_dict({**recipe, 'work_long_side': 1280, 'flux3_generation': {'marker': True}}).work_long_side, 1280)
        with self.assertRaises(ContractValidationError):
            RenderRecipe.from_dict({**get_pinned_recipe(RECIPE_PROD_SDNQ).to_dict(), 'flux3_generation': {'marker': True}})

    def test_validated_job_forwards_the_generation_choice_to_the_runner(self):
        from deploy.cloud.common.flux import render_job
        from deploy.cloud.common.contract import provisional_fixture_limits
        meta = JobRequestMetadata.from_dict(json.loads(GENERATION_FIXTURE.read_text()))
        image = (FIXTURE.parent / 'tiny_image.png').read_bytes()
        hint = (FIXTURE.parent / 'tiny_hint.png').read_bytes()
        class RecordingRunner:
            def render_png(inner, *args, **kwargs):
                inner.kwargs = kwargs
                return image
        runner = RecordingRunner()
        render_job(runner, meta, image, hint, provisional_fixture_limits())
        self.assertEqual(runner.kwargs['flux3_generation'], meta.recipe.flux3_generation)
        self.assertNotIn('work_long_side', runner.kwargs)

if __name__ == '__main__': unittest.main()
