# Cloud configuration memory

Cleaning and page denoising capabilities are saved separately for each deployment
in `settings.json`, under `cloudConfigurationMemory`. The identity includes the
provider, profile ID, endpoint URL and profile update timestamp. Selecting another
profile restores its own capabilities; editing or redeploying a profile gives it
a new identity and checks it again. Cached denoising records include the supported
preset IDs, and cleaning records include the advertised model ID.

Metadata checks send no pages and start no GPU. Saved answers survive app restarts
and remain available when a deployment is unreachable. Denoising capability
requests name the profile explicitly, so switching the default during a request
cannot associate the response with another deployment.

A definitive setup failure, missing cleaning weights, invalid endpoint or access
failure marks the affected operation unavailable and notifies the user. Failures
from running work update the deployment that work started on, even after switching
profiles. Network failures, GPU availability failures and unsupported page formats
do not erase capabilities. No failed operation is automatically sent somewhere
else.

A cleaning record has one of three states. `ready` names the model. `missing` is
a setup that needs repair, and it warns. `none` is a deployment set up without a
cleaning model: the metadata read stores it with no warning
(`checkCleanConfiguration` in `src/lib/state/cloudconfig.svelte.js`), cloud
cleaning is not offered on it, and a clean pointed at it explains that the
deployment has no cleaning model and how to add one
(`cloud.configuration.cleanNotInstalled`). Denoising and cloud detection on that
deployment are not affected.

After repairing setup or access, use the endpoint's connection test in Settings >
Cloud. A successful connection test rechecks cleaning metadata and denoising
presets and updates memory. A save failure is reported; the updated observation
remains available for the current app session.

See also: [cloud-frontend.md](cloud-frontend.md), [cloud-contract.md](cloud-contract.md), [cloud-provisioning.md](cloud-provisioning.md).
