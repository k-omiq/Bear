/**
 * The English catalogue.
 *
 * Nested by area, one nesting level per key segment, so a key like
 * `masks.value.cloudCost` is `en.masks.value.cloudCost`. `index.js` flattens it
 * once at module load; nothing walks this object at render time.
 *
 * Rules the copy follows, and a translator inherits:
 *
 * - **Named placeholders, never concatenation.** `{count}`, `{path}`, `{name}`.
 *   Word order is the translator's to change.
 * - **A `{…Key}` placeholder carries another key**, resolved before it is
 *   interpolated. `{statusKey}` is a key; `{status}` would be a literal.
 *   Task 3's notices depend on this and so does every provenance row.
 * - **A `{name:format}` placeholder is formatted, not stringified.** The only
 *   format today is `currency`, and `masks.value.cloudCost` is the only place
 *   money is written. Nothing else in the app may print a `$`.
 * - **A string may be an object of plural forms** - `{zero, one, other}`,
 *   selected against `params.count`. `zero` is not an English `Intl.PluralRules`
 *   category; `t()` honours it anyway, because "0 regions need review" is a bad
 *   sentence and the zero case is the common one.
 * - **Proper nouns are not translated.** Licence identifiers, URLs, provider
 *   and model names, format names (PNG, JPEG, CBZ) and version strings arrive
 *   from the backend as values, and are interpolated, not looked up.
 * - **Errors name the problem and the recovery.** A refusal says what it
 *   refused and what to do instead.
 * - No emoji. No string assembled from fragments in a component.
 */

export const en = {
  qwen: {
    "prompt": {
        "title": "Describe what to erase",
        "help": "Pick the lettering type. You can add a short description of what it looks like or where it is. No prompt-writing needed.",
        "batch": "These choices apply to every selected region. Choose Automatic for mixed lettering.",
        "target": "Lettering type",
        "auto": "Automatic",
        "dialogue": "Dialogue",
        "soundEffect": "Sound effect",
        "otherText": "Other",
        "description": "Description (optional)",
        "example": "Big black letters beside the hand.",
        "preserve": "Manga Cleaner adds instructions to preserve the artwork, bubble outlines, and surrounding details. You will review the result before it is applied.",
        "cancel": "Cancel",
        "clean": "Clean with Qwen",
        "updateRequired": "Update the Qwen cloud deployment to use descriptions and the 8-step erase recipe."
    },
    "review": {
        "title": "Review Qwen clean",
        "help": "Check that the lettering is gone and the artwork is intact. Use result applies the blended preview to the page.",
        "before": "Before",
        "after": "After",
        "retryCost": "Trying again makes another paid cloud request.",
        "discard": "Discard",
        "retry": "Try again",
        "use": "Use result",
        "expired": "This review has expired. The result was not applied.",
        "failed": "Qwen did not make a sufficient edit after three seeds. Describe the lettering and try again.",
        "required": "This Qwen result is cached and needs review before it can be applied."
    }
},
  /* ================================================================== */
  /* app - the product's own name                                        */
  /* ================================================================== */
  app: {
    name: {
      mangaCleaner: 'Manga Cleaner',
    },
  },

  /* ================================================================== */
  /* shell - dialog and notice furniture that belongs to no one screen   */
  /* ================================================================== */
  shell: {
    action: {
      cancel: 'Cancel',
      close: 'Close',
      done: 'Done',
      confirmSpend: 'Send to the cloud',
      convert: 'Convert',
      dismissNotice: 'Dismiss',
      // Beside a path field, never instead of one: the field can also be typed
      // into, and outside the desktop window there is no chooser to open.
      chooseFolder: 'Choose folder…',
    },
  },

  /* ================================================================== */
  /* modal - dialog titles, and the bodies of the dialogs that are one   */
  /* statement plus two buttons                                          */
  /* ================================================================== */
  modal: {
    title: {
      settings: 'Settings',
      export: 'Export',
      shortcuts: 'Keyboard shortcuts',
      newProject: 'New project',
      newChapter: 'New chapter',
      deleteChapter: 'Delete chapter',
      openProject: 'Open project',
      renameProject: 'Rename project',
      removeProject: 'Remove this project?',
      replaceDenoised: 'Replace pages with denoised pages?',
      formatConversion: 'Convert to an editable format?',
      cloudConsent: 'Send this region to your cloud GPU?',
      overwriteRefusal: 'Refusing to overwrite the source',
    },
    // The one question before a project's first cloud render. Three facts: what leaves
    // the machine, where it goes, what it costs. The crop's surrounding pixels
    // are the point (the reconstruction needs the ring around the text), so
    // the first fact says it is more than the masked region. It says nothing
    // about the rest of the project staying here: confirming stands for later
    // requests too (`cloud.projectConsent`).
    cloudConsent: {
      what: 'What is sent',
      whatValue: 'A {width} by {height} pixel crop around this region and its mask.',
      where: 'Where it goes',
      whereValue: '{name}, your endpoint on {providerKey}',
      whereUnnamed: 'Your endpoint on {providerKey}',
      cost: 'Cost',
      costEstimate: 'About {cost:currency}, billed by the provider to your account.',
      costUnknown: 'No estimate. The provider bills your account for the GPU time this render uses.',
    },
    body: {
      formatConversion: {
        one: 'Convert one {from} file to {to} so it can be edited? The original stays.',
        other: 'Convert {count} {from} files to {to} so they can be edited? The originals stay.',
      },
    },
    note: {
    },
  },

  /* ================================================================== */
  /* settings - the Settings dialog's own rows                           */
  /* ================================================================== */
  settings: {
    // General's three groups.
    general: {
      appearance: 'Appearance',
      reading: 'Reading',
      app: 'App',
    },
    background: {
      label: 'Keep running when closed',
      description: 'The window hides while downloads continue. Use the tray icon to reopen or quit. Off: the close button quits the app.',
      saveFailed: 'Could not save the close behavior. Try again.',
    },
    theme: {
      label: 'Theme',
      light: 'Light',
      dark: 'Dark',
      system: 'System',
      sakura: 'Sakura',
      jade: 'Jade',
      ocean: 'Ocean',
    },
    direction: {
      label: 'Reading direction',
      rtl: 'Right to left',
      ltr: 'Left to right',
    },
    // How the canvas draws a detected region's mask: the area Clean will
    // erase. The outline is always at full strength; opacity is the fill.
    // Two colours, one for text in a speech bubble and one for text outside
    // bubbles, in the words the Text cleanup rows use for the same two.
    mask: {
      color: 'Selection color, speech bubble text',
      outsideColor: 'Selection color, text outside bubbles',
      opacity: 'Selection opacity',
      description: 'How detected text areas are drawn on the page before Clean.',
      percent: '{value}%',
    },
    // The cloud setup the Cloud section holds. The switch itself is
    // `settings.inference.permission`.
    cloud: {
      setup: {
        title: 'Set up a cloud GPU',
        heading: {
          connect: 'Connect your account',
          review: 'Review the setup',
          running: 'Setting up',
          cleaning: 'Deleting cloud resources',
          stopped: 'Setup stopped',
          failed: 'Setup did not finish',
          cleanupFailed: 'Some resources were not deleted',
          done: 'Your cloud GPU is ready',
          finished: 'Setup finished',
          resume: 'Resume setup',
          cleanup: 'Delete cloud resources',
          cleaned: 'Cloud resources deleted',
          found: 'Already in your account',
          existing: 'You already have a cloud GPU',
          update: 'Update your cloud GPU',
        },
        // A new setup on a computer that already has one: update it instead.
        // Update → Change options: new choices for a setup this computer made.
        update: {
          options: 'Change options',
          optionsNote: 'To turn on page denoise or pick another GPU or idle time, choose Change options. Update alone keeps the current choices.',
          denoiseNote: 'Page denoise is added to this setup. Continue shows the plan to approve.',
          notFound: 'This setup was not found in your Modal account. Check that the key is for the same account.',
        },
        guard: {
          lead: {
            one: 'This computer already has a cloud GPU set up. Update it instead of setting up another?',
            other: 'This computer already has {count} cloud GPUs set up. Update one instead of setting up another?',
          },
          legend: 'Cloud GPUs set up on this computer',
          note: 'Updating keeps its storage and the models it downloaded. A new setup creates another endpoint and storage in your account, downloads the models again and adds storage cost. To add another Modal account, choose Set up a new one and enter that account’s key.',
          update: 'Update',
          another: 'Set up a new one',
        },
        // Installations `inspect` found in the account, offered before a new one.
        found: {
          lead: {
            one: 'Your {providerKey} account already has a Manga Cleaner setup. Use it to skip the download and avoid paying to store the models twice.',
            other: 'Your {providerKey} account already has {count} Manga Cleaner setups. Use one to skip the download and avoid paying to store the models twice.',
          },
          legend: 'Setups in your account',
          models: 'Downloaded: {models}',
          noModels: 'The model download did not finish.',
          unchecked: 'Could not check which models are downloaded.',
          options: {
            one: '{gpu} GPU, stays on {count} minute after a render',
            other: '{gpu} GPU, stays on {count} minutes after a render',
          },
          here: 'Set up on this computer',
          created: 'Set up on {date}',
          partial: 'Only the newest setups are listed.',
          new: 'Set up a new one',
          newNote: 'A new setup creates another endpoint and storage, downloads about {size} GB of models again and adds storage cost.',
          use: 'Use this setup (no new download)',
          useUnready: 'Use this setup',
          update: 'Update this setup',
        },
        connect: {
          lead: 'Setup creates a private GPU endpoint in your own account. The provider bills you for it directly.',
          provider: 'Provider',
          modalNote: 'Paste Modal’s token command or enter its ID and secret.',
          beamNote: 'Needs an API key.',
          pausedTag: 'Paused',
          pausedNote: 'Beam is paused in this version. Use Modal.',
          modalTokenId: 'Modal token ID',
          modalTokenSecret: 'Modal token secret',
          modalCommand: 'Paste Modal token command',
          modalCommandOr: 'Or enter the token ID and secret separately.',
          modalCommandInvalid: 'Paste the complete “modal token set” command, including its token ID, secret, and profile.',
          modalCommandImported: 'Token imported from Modal profile “{profile}”.',
          beamToken: 'Beam API key',
          help: 'Where do I find this?',
          helpModal: 'Sign in at modal.com, open Settings, then API Tokens, and create a new token. Copy the “modal token set” command and paste it here. The app reads it without running a shell.',
          helpBeam: 'Sign in at beam.cloud, open Settings, then API Keys, and create a key. Copy it when Beam shows it.',
          copyLink: 'Copy link',
          copied: 'Copied',
          keyNoteModal: 'Setup makes a separate access token that can only call the new endpoint. Your Modal token is kept only if you choose to remember it.',
          rememberKey: 'Remember this Modal token in this computer’s keychain, so updates do not ask for it again',
          savedKey: 'Uses the Modal token saved on this computer.',
          otherKey: 'Use another token',
          keyNoteBeam: 'Beam endpoints are called with your own API key, so the app keeps it in this computer’s keychain. Nothing else stores it.',
          notEligible: 'This account cannot run a GPU endpoint yet. Check that GPU access and billing are turned on with the provider, then try again.',
          continue: 'Continue',
          checking: 'Checking…',
        },
        review: {
          lead: 'Setup will create these in your {providerKey} account:',
          workspace: 'Workspace',
          model: 'Cloud model',
          modelNote: 'Choose the cloud model, GPU and idle time before you approve setup. Changing the model downloads a separate checkpoint.',
          modelLicense: 'Model license: {license}. Check its terms before using it for your work.',
          analysisModels: 'Cloud analysis models',
          analysisNote: 'Select Ogkalu comic text & bubble detector (Full), SAM-TS-L lettering mask, or both for cloud detection. CTD and the Small detector run on this computer only. Setup installs the selected graphs; GPU analysis starts only when requested.',
          analysisWeights: 'Selected analysis graphs add about {size} GB of stored data. The SAM-TS-L lettering mask also uses CPU time to export its graphs during setup.',
          denoise: 'Page denoise',
          denoiseInstall: 'Set up page denoise',
          denoiseNote: 'Adds the six denoise models, about {size} GB of stored data. Denoise uses the same on-demand GPU and rate as cloud analysis.',
          gpu: 'GPU',
          idle: 'Stay on after a render',
          idleOption: {
            one: '{count} minute',
            other: '{count} minutes',
          },
          region: 'Connect through',
          regionNote: 'Your requests and results travel through Modal’s servers in this place. Choose the one nearest to you. A change gives this setup a new address.',
          regionUsEast: 'US East (Virginia)',
          regionUsWest: 'US West (Oregon)',
          regionEuWest: 'Europe (Dublin)',
          regionApSouth: 'Asia (Mumbai)',
          weights: 'Setup downloads about {size} GB of model weights into your account. This happens once.',
          costGpu: '{providerKey} bills your account for GPU time while a render runs.',
          costIdle: 'After each render the GPU stays on for the time above, which is billed too, and then stops. Storing the model weights may add a small charge.',
          tokenModal: 'Setup also creates an access token that can only call this endpoint. The app keeps it in this computer’s keychain.',
          tokenBeam: 'Beam has no separate access token. The endpoint is called with your own Beam API key, which the app keeps in this computer’s keychain. Setup also stores the key as a secret in your Beam account, so the gateway can start GPU jobs.',
          approve: 'I approve creating these in my {providerKey} account and paying for what they use.',
          leadReuse: 'Setup reuses {id} in your {providerKey} account, with its storage and the models already downloaded. It updates these and creates only a new access token:',
          weightsReused: 'The models are already in this setup’s storage. Nothing downloads again.',
          weightsBeside: 'The selected model is not in this setup’s storage yet. About {size} GB downloads beside the models already there, which adds storage cost.',
          analysisBeside: 'The main model is already in this setup’s storage. Only the selected analysis graphs that are missing download.',
          denoiseBeside: 'The main model is already in this setup’s storage. The page denoise models and any missing analysis graphs download.',
          weightsUnknown: 'The app could not check this setup’s storage. Setup downloads only what is missing.',
          optionsUnknown: 'This setup did not record its GPU and idle time, so the choices below are the defaults. Keep the model it already has to avoid a second download.',
          tokenReuse: 'Setup creates an access token for this computer and keeps it in the keychain. A token another computer made for this setup keeps working until you remove it in Modal, under Settings, Proxy Auth Tokens.',
          approveReuse: 'I approve updating this setup in my {providerKey} account and paying for what it uses.',
          startReuse: 'Use this setup',
          details: 'Technical details',
          hash: 'Plan hash',
          installation: 'Installation ID',
          back: 'Back',
          start: 'Start setup',
        },
        running: {
          lead: 'This can take several minutes the first time. You can close Settings; setup keeps going.',
          cleanupLead: 'Deleting what setup created. You can close Settings; this keeps going.',
          starting: 'Starting…',
          elapsed: 'Elapsed {time}',
          stop: 'Stop',
          stopping: 'Stopping…',
        },
        // Read after each step's name by a screen reader; the checklist's
        // marks say the same thing to the eye.
        state: {
          running: 'in progress',
          done: 'done',
          fail: 'failed',
          skip: 'skipped',
        },
        failed: {
          code: 'Code: {code}',
          kept: 'What finished is kept. Resume picks up from there.',
          keys: 'Enter your key again to resume or clean up.',
          resume: 'Resume',
          cleanup: 'Clean up',
          retry: 'Try again',
          again: 'Start again',
        },
        done: {
          saved: 'Saved and selected as your cloud GPU: {name}.',
          unchecked: '{name} is saved and selected as your cloud GPU. Test it from Settings > Cloud.',
          fallbackName: '{providerKey} ({id})',
          tryIt: 'Try it: choose Cloud as the engine in the tool bar. The first render can take 1 to 3 minutes while the GPU starts.',
          nothing: 'That setup had already finished. Nothing was changed.',
        },
        resume: {
          lead: 'A setup in your {providerKey} account did not finish ({id}). Enter your key to pick up where it stopped.',
          reissue: 'Enter your {providerKey} key to give this endpoint ({id}) a new access token. Setup checks what is already there and only redoes what is missing.',
          update: 'Enter your {providerKey} key to update {id}. Setup redeploys it with this version of the app and gives this computer a new access token. Nothing downloads again.',
        },
        cleanup: {
          planning: 'Checking what to delete…',
          none: 'Nothing that setup created is left in your {providerKey} account.',
          lead: 'These will be deleted from your {providerKey} account, including the downloaded model weights:',
          ignored: {
            one: 'One other resource in the account was not made by this setup and is left alone.',
            other: '{count} other resources in the account were not made by this setup and are left alone.',
          },
          approve: 'I understand these are deleted for good, and a new setup downloads the weights again.',
          forget: 'Forget this setup',
          delete: 'Delete',
          done: 'Deleted from your {providerKey} account. The endpoint is removed from this computer too.',
          doneEmpty: 'Nothing was left in your {providerKey} account. The endpoint is removed from this computer.',
        },
        // One per IC-2 step id, in the order the helper runs them.
        step: {
          inspect: 'Check the account',
          validate: 'Check the plan',
          volume: 'Create storage for the model',
          state: 'Create job storage',
          secret: 'Store the endpoint’s secret',
          image: 'Build the container image',
          deploy: 'Deploy the endpoint',
          weights: 'Download the model weights',
          token: 'Create an access token',
          endpoint: 'Save the endpoint',
          health: 'Check the endpoint',
          cleanup: 'Delete resources',
          working: 'Working',
        },
        resource: {
          volume: 'Storage for the model weights',
          app: 'GPU endpoint',
          token: 'Access token',
          state: 'Job storage',
          gateway: 'Gateway',
          worker: 'GPU worker',
          secret: 'Secret',
          unknown: 'Other resource',
        },
        // One per helper error code. The helper's own message is never shown:
        // provider errors can carry a key or a signed URL.
        error: {
          permission: 'Your key does not have permission for this. Check that it can create apps and storage, then try again.',
          validation: 'The details were not accepted. Check the key and try again.',
          planChanged: 'The plan changed after you reviewed it, so nothing was created. Start again to review the new plan.',
          // The same refusal after Resume or Update, which show no plan: there
          // it means the key is for another account.
          wrongAccount: 'Your key does not match the account this setup was made in, so nothing was changed. Use a key for that account, then choose Resume.',
          platform: 'This account cannot run GPU endpoints yet. Check GPU access and billing with the provider.',
          unavailable: 'The provider could not be reached. Check your connection and try again.',
          helperMissing: 'This Manga Cleaner installation is missing its cloud setup helper. Install a complete app build, then try again.',
          failed: 'Something went wrong during setup.',
          timeout: 'Setup took too long and was stopped.',
          secretStore: 'The access token could not be saved in this computer’s keychain. Resume creates a new one.',
          configWrite: 'The endpoint could not be saved on this computer. Resume tries again.',
          cancelled: 'You stopped setup.',
          request: 'The app and its setup helper did not understand each other. Update the app and try again.',
          cleanup: 'Some resources could not be deleted. Try again, or delete them in your provider’s dashboard.',
          cleanupWrongAccount: 'Your key does not match the account this setup was made in, so nothing was deleted. Use a key for that account, then try again.',
          orphanedToken: 'Setup may have created a Modal access token without recording its ID, so it cannot resume safely.',
          generic: 'Setup did not finish.',
        },
        // The way out of `orphanedToken`, in order. Catalogue text only: the
        // helper sends its own steps, which are never shown for the reason
        // `error` gives.
        orphaned: {
          heading: 'To recover',
          dashboard: 'Sign in at modal.com, open Settings, then Proxy Auth Tokens, and remove the token this setup created. Resume stays blocked, because the app does not know that token’s ID.',
          cleanup: 'Press Clean up to delete the rest of what this setup created.',
          again: 'Start a new setup from Settings, Cloud.',
        },
      },
    },
    originalView: {
      label: 'Original view',
      hold: 'Hold only',
      pinned: 'Pinned',
    },
    language: {
      label: 'Language',
    },
    // Inside Models > Cleaning's collapsed "AI redraw (FLUX)", so the labels
    // do not repeat it. The summary uses the backend's helper availability.
    sidecar: {
      heading: 'AI redraw (FLUX)',
      ready: 'Set up',
      notSetUp: 'Not set up',
      label: 'Folder',
      automaticFolder: 'Find automatically',
      chooserTitle: 'AI redraw (FLUX) folder',
      notFound: 'No engine found in this folder.',
      install: 'Install FLUX helper and 4B model',
      installing: 'Installing FLUX…',
      installFailed: 'FLUX setup failed: {detail}',
      accelerator: 'GPU runtime for installation',
      acceleratorAuto: 'Automatic',
      mlxAutomatic: 'MLX uses the Apple GPU automatically.',
      stage: {
        environment: 'Creating the helper environment…',
        dependencies: 'Installing the model runtime…',
        weights: 'Downloading model weights…',
        ready: 'Finishing setup…',
      },
    },
    fluxBackend: {
      label: 'Backend',
      auto: 'Automatic',
      mflux: 'MLX (Apple)',
      mfluxUnsupported: 'MLX (Apple Silicon only)',
      mfluxReason: 'MLX requires an Apple Silicon Mac. Choose Automatic or SDNQ here.',
      sdnq: 'SDNQ (CUDA, Intel XPU, or Apple Metal)',
    },
    sidecarModel: {
      label: 'Model',
      noneFound: 'No models found in weights folder.',
      // The stored model, when the helper no longer lists it. Drawn as an
      // option so the picker does not show a different model than the one set.
      missing: '{id} (not found)',
    },
    // Models > Detection: what each model is for, which workflow needs it,
    // and what removing it stops. Setup reuses the lines that describe the
    // same choice.
    detection: {
      languagesAllText: 'Text cleanup takes all text, so language filtering is off. Set Text to Chosen languages in Text cleanup to use it.',
      // Under the four choices.
      profiles: 'Full and Small are two sizes of one detector: choose one. CTD and the SAM-TS-L lettering mask combine with either.',
      selectedModels: 'Selected combination: {models}',
      download: 'Download {bytes:memory}',
      capability: {
        detect: 'Detection models',
        japanese: 'Language filtering',
      },
      // The collapsed language filtering's summary: whether it runs.
      filtering: {
        on: 'On, for the languages chosen below',
        off: 'Off while Text cleanup takes all text',
      },
      // Setup's download row for the lettering mask.
      model: {
        samTs: 'SAM-TS-L lettering mask',
      },
      role: {
        ctd: 'Finds lettering for automatic cleaning and page review.',
        rtSmall: 'Finds speech bubbles and text regions for automatic cleaning and page review.',
        rtFull: 'Finds larger page regions with the full two-tile profile. Downloaded when selected.',
        samTs: 'Draws lettering pixels for automatic cleaning and page review, with no OCR. Setup downloads and exports its graphs locally.',
        scriptGate: 'Keeps legacy cleaning to the source languages you choose. All-text cleaning does not use it.',
        mangaOcr: 'Optionally resolves uncertain Japanese script decisions during legacy cleaning.',
        hayaiOcr: 'Optionally reads each region before cleaning, to hold art that is not text and to confirm unclear Japanese, Chinese and Korean script. Works under both policies.',
      },
      // Under the OCR rescue switch, only while it is on and cannot run.
      rescue: {
        missing: 'The text reader files are not installed, so the reader will not run.',
        size: 'Adds a {bytes:memory} download.',
      },
      // One line under the policy: whether the selected workflow can run.
      ready: {
        nothing: 'Every language is skipped, so legacy cleaning removes nothing.',
        legacy: 'Legacy cleaning has every model it needs.',
        legacyMissing: 'Legacy cleaning needs {bytes:memory} of downloads before it can run.',
        allText: 'All-text automatic cleaning has every selected detection model it needs.',
        allTextMissing: 'All-text cleaning needs {bytes:memory} of downloads.',
        allTextImport: 'All-text cleaning needs the SAM-TS-L lettering mask graphs. Install them on its row below.',
        allTextSamMismatch: 'The SAM-TS-L lettering mask graphs failed their checksum. Install them again on its row below.',
        // Free memory changes, so this says what to do rather than what is missing.
        allTextMemory: 'SAM-TS-L lettering mask needs about 10 GB of free memory, and this computer has less free right now. Close other apps before cleaning or reviewing.',
        // After the line above, so "it" is the workflow that line names. A
        // native run refuses to start without the runtime, so the row is not
        // complete while one of these shows.
        runtime: 'It cannot run until the engine runtime, ONNX Runtime, is installed. Download it in Performance.',
        runtimeDownloading: 'It can run once the engine runtime, ONNX Runtime, finishes downloading.',
        runtimeUnavailable: 'It cannot run here: the engine runtime, ONNX Runtime, has no build for this computer.',
        // Installed is a file found; a run also has to load it. `reasonKey`
        // is a `diagnostics.runtime.*` sentence, which names the remedy where
        // there is one outside this app.
        runtimeUnloadable: 'It cannot run until the engine runtime loads. {reasonKey}.',
        runtimeChecking: 'Checking that the engine runtime, ONNX Runtime, loads on this computer.',
        runtimeUnchecked: 'Whether the engine runtime, ONNX Runtime, loads on this computer could not be checked. Reopen Settings to check again.',
        openPerformance: 'Open Performance',
      },
      review: {
        summary: 'Text-shaped review',
        optional: 'Optional mode',
        note: 'Analyses one page with the selected detection models. Any component write still needs explicit approval and qualified write support.',
      },
      sam: {
        memory: 'Needs about 10 GB of free memory to run.',
        chooserTitle: 'Folder with both SAM-TS-L lettering mask graphs',
      },
      rtFull: {
        chooserTitle: 'The pinned Ogkalu comic text & bubble detector (Full) detector.onnx',
      },
      // Setup's detection step.
      setupReview: 'The selected models can also check one page at a time from the editor’s review, before anything is erased.',
      modelsLegend: 'Detection models',
      // Why Text cleanup's Detect on cannot be Cloud GPU, and what a cloud
      // choice means for a model's backend. Detect on itself is the Text
      // cleanup panel's; region edits and drawn boxes stay on this computer.
      runOn: {
        off: 'Cloud GPU needs cloud engines turned on in Settings > Cloud.',
        notReady: 'Cloud GPU needs an endpoint with a stored key in Settings > Cloud.',
        notOffered: 'Your cloud GPU does not offer this model. Update the cloud worker to add it.',
        unread: 'Could not read which models your cloud GPU offers. Check the endpoint in Settings > Cloud.',
        checking: 'Checking which models your cloud GPU offers…',
        performance: '☁ Runs on your cloud GPU while Text cleanup detects there, so this backend is not used.',
      },
      setupAllText: 'Automatic cleaning will use this model combination across the page, without source-language filtering.',
    },
    cleaning: {
      capability: {
        rebuild: 'Rebuild background',
      },
      role: {
        lama: 'Redraws screentone and texture under removed text whenever a clean uses LaMa.',
      },
    },
    // The weights and the ONNX Runtime are not bundled: they are downloaded
    // after install. Everything here is about
    // *files on this machine*, so the copy names sizes and folders and never
    // talks about "AI" - the reader is deciding what to spend disk on.
    models: {
      // Over catalogue rows no capability claims: a weight the backend added
      // before the capability table knew it still has a place to be managed.
      heading: 'Other files',
      // Where a detection choice runs, under its name. While Text cleanup
      // detects on the cloud GPU the combination is fixed
      // (`pipelines.js#runDetection`): Full and SAM-TS-L run there
      // (`onCloud`), CTD here beside them (`cloudLocal`), Small not at all
      // (`cloudUnused`).
      where: {
        either: 'Runs on this computer or your cloud GPU',
        localOnly: 'Runs on this computer only',
        onCloud: '☁ Runs on your cloud GPU while Text cleanup detects there',
        cloudLocal: 'Runs on this computer beside the cloud GPU while Text cleanup detects there',
        cloudUnused: 'Not used while Text cleanup detects on the cloud GPU: Full replaces it',
      },
      // Every model downloads from Hugging Face, so the token has its own
      // group rather than living under either pipeline.
      access: {
        heading: 'Download access',
      },
      // One logical row per multi-file model. The download notices name a
      // group failure with the same key.
      groups: {
        scriptGate: 'Script filtering',
        mangaOcr: 'Japanese OCR rescue',
        hayaiOcr: 'Text reader (Hayai OCR)',
      },
      fileCount: {
        one: '{count} file',
        other: '{count} files',
      },
      // The expandable block under a model row: component files, their exact
      // identity, and per-file Check and Delete for troubleshooting.
      details: 'File details',
      revision: 'Revision {revision}',
      revisionUnavailable: 'No upstream revision is recorded. The pinned SHA-256 is the exact identity.',
      importPair: 'Import both graphs together from an export you obtained yourself.',
      groupMismatch: 'At least one file did not match its pinned digest.',
      // Beside a single file's Delete in File details, held because another
      // workflow shares the file and the selected one uses it now.
      fileShared: 'Another workflow shares this file, and the selected policy uses it now. To remove it anyway, use Delete on the row above, which names what stops.',
      // A removal names what it stops before it happens. The shared speech
      // bubble finder is never removed with another model.
      remove: {
        keep: 'Keep',
        inUse: 'Automatic cleaning uses it now.',
        ctd: 'Delete Comic Text Detector (CTD)? Any selected cleaning or review combination that uses it stops until you download it again.',
        rtSmall: 'Delete Ogkalu comic text & bubble detector (Small)? Any selected cleaning or review combination that uses it stops until you download it again.',
        rtFull: 'Delete Ogkalu comic text & bubble detector (Full)? Any selected cleaning or review combination that uses it stops until you download it again.',
        samTs: 'Delete the SAM-TS-L lettering mask graphs? Any selected cleaning or review combination that uses them stops until you install them again.',
        scriptGate: 'Delete script filtering, both files? Legacy automatic cleaning stops working until you download it again. The speech bubble finder is kept.',
        mangaOcr: 'Delete the Japanese OCR rescue, all three files? Legacy cleaning keeps running without the rescue. The speech bubble finder is kept.',
        hayaiOcr: 'Delete the text reader, all three files? Cleaning keeps running without it.',
        lama: 'Delete LaMa Manga? LaMa Manga cleaning stops working until you download it again.',
        // Not `other`: that name would read the block as plural forms.
        file: 'Delete {name}? Anything that needs it stops working until you download it again.',
      },
      status: {
        installed: 'Installed',
        missing: 'Not installed',
        // A model row whose files are only partly here.
        someInstalled: '{installed} of {total} files installed',
        // Said beside a missing model the selected workflow needs, in words
        // as well as colour.
        neededNow: 'needed now',
        // The SAM-TS-L lettering mask and the Full detector, which also take files imported from disk.
        importToEnable: 'Import to enable',
        imported: 'Imported',
        verified: 'SHA-256 verified',
        checking: 'Checking…',
        readinessUnavailable: 'Readiness unavailable',
        downloading: 'Downloading…',
        downloadingPercent: 'Downloading {percent}%',
        // The check found different bytes under the right name. Not the same
        // as "not installed": the file is there and must not be trusted.
        mismatch: 'Check failed',
        failed: 'Download failed',
        // Found outside the folder this app writes to - a bundled copy, or a
        // developer's checkout. It works; it is not ours to delete.
        readOnly: 'installed elsewhere',
        // What a stopped download left on the disk. The bytes
        // are kept so the next Download picks up where it stopped, which is
        // the half the reader has to be told before Discard means anything -
        // otherwise the button reads as "delete something I might need".
        partial: '{bytes:memory} downloaded. Download continues from there.',
      },
      action: {
        download: 'Download',
        install: 'Install automatically',
        cancel: 'Cancel',
        delete: 'Delete',
        // Throws away the unfinished download the line above reports, and
        // nothing else: the installed file, if there is one, is untouched.
        discard: 'Discard partial',
        // Re-reads the whole file and compares it against the published
        // checksum. Seconds of work on the larger models, which is why it is a
        // button rather than something that happens every time this opens.
        verify: 'Check',
        // Opens a file chooser for a local, hash-checked import.
        import: 'Import…',
      },
      runtime: {
        label: 'Engine runtime',
        // No published build for this platform. An Intel Mac, today.
        unavailable: 'Not available for this computer',
        // The build to download, where a platform publishes more than one.
        // Windows and Linux x64 do - DirectML or the WebGPU plugin for every
        // graphics card, CUDA for NVIDIA - and a Mac does not, so this row is
        // not drawn there.
        flavour: 'Runtime build',
        // The build names are ids and the versions are data, so neither is
        // translated; what needs saying is that one of the choices asks the
        // user to install something first.
        flavourNeeds: 'This build needs {items} installed on this computer before it can use the graphics card.',
        // Said **only when the two differ**. The row's build is
        // the one a Download press would fetch, and until the press happens the
        // computer is still running the one it has - two true statements that
        // read as one false one when only the first is on screen. The build
        // names and the versions are ids and data, so they are interpolated
        // rather than translated.
        installedDiffers:
          'Installed: {installed} {installedVersion}. Chosen: {chosen} {chosenVersion}. Press Download to replace it.',
      },
      // What a press answered when it did nothing. None of these is a failure:
      // each is this window looking at a row that another window has already
      // moved past.
      declined: {
        alreadyRunning: 'This is already downloading.',
        alreadyInstalled: 'This is already installed.',
        notFound: 'There was nothing to delete.',
        readOnlyElsewhere: 'This copy is outside the folder this app manages, so it was left alone.',
        // A delete refused because a download is writing into the same folder.
        // The only row that can reach it is the runtime's.
        busy: 'A download is using this folder. Cancel it first, then delete.',
      },
      folder: 'Downloads go to {path}',
      unavailable: 'This build cannot manage downloads.',
      token: {
        label: 'Hugging Face token',
        placeholder: 'hf_…',
        save: 'Save',
        clear: 'Clear',
        saved: 'A token is saved in this computer’s credential store.',
        // The fallback, in its two forms. They read almost the
        // same and mean opposite things: one is a computer that cannot do
        // better, the other is a keychain that would work if it were unlocked.
        // Saying the first when it is the second sends the
        // user looking for a problem that is not there.
        savedInFile: 'A token is saved. This computer has no credential store, so it is kept in the settings file as plain text.',
        savedStoreUnreachable: 'A token is saved. This computer’s credential store could not be reached, so it is kept in the settings file as plain text. Unlock the keychain and save again to move it.',
        fileOnly: 'This computer has no credential store, so a token would be kept in the settings file as plain text.',
        storeUnreachable: 'This computer’s credential store could not be reached, so a token would be kept in the settings file as plain text.',
        // A Clear the credential store refused. The secret is still in it, and
        // the only place it can be removed from is the user's own keychain.
        clearFailed: 'The token could not be removed from this computer’s credential store. It is still there. Remove it with the system keychain tool.',
        // Every other way a Clear can fail: an unwritable settings file, a
        // backend that is not answering. The keychain sentence above is a
        // specific and alarming claim - "your secret is still there" - and
        // saying it for a disk error sends the user to the wrong tool.
        clearFailedOther: 'The settings could not be written, so nothing was changed. Try again.',
        // **Why** the store could not be reached, in the user's terms rather
        // than the platform's. One line under the sentence
        // above, and each one is a different instruction: the first is the only
        // one the user can act on where they are standing.
        reason: {
          locked: 'It looks locked. Unlock it and press Save again.',
          unreachable: 'The credential service did not answer on this computer.',
          ambiguous:
            'It holds more than one entry for this application and cannot tell which token is this one’s. Remove the extra entries with the system keychain tool.',
          unknown: 'It refused without saying why.',
        },
      },
    },
    accel: {
      label: 'Graphics acceleration',
      auto: 'Automatic',
      inherit: 'Use global default ({backend})',
      models: 'Model execution',
      // The collapsed per-model list's summary.
      modelsCount: {
        one: '{count} model',
        other: '{count} models',
      },
      modelHelp: 'Each local model can inherit the global choice or use its own backend. Changes apply to the next inference session.',
      modelLabel: 'Local backend for {model}',
      predicted: 'Next session: {backend}',
      refused: '{backend} is unavailable for this model: {reason}. The next run will stop until you choose another backend.',
      unavailableForModel: '{model} cannot use {backend}: {reason}. Choose a supported backend in Performance.',
      // Setup's backend step, beside the two models with a cloud version.
      cloudReview: '☁ Can also run on your cloud GPU: choose Cloud GPU for Detect on in Text cleanup.',
      saveFailed: 'Could not save the model backend. Try again.',
      state: {
        supported: 'supported; install the runtime or dependency',
        installed: 'installed; check the device or dependency',
        available: 'available; execution has not been verified',
        verified: 'verified by inference',
        unsupported: 'unsupported for this model',
      },
      // The picker with nothing in it. `listAccelerators` is the engine runtime
      // being asked what this machine can run a model on, so a rejection is
      // almost always the runtime itself failing to load - and the runtime's
      // own row, above it in Performance, is where the reason is written.
      // This sentence sends the reader there rather than restating it, because
      // two explanations of one fault are two things that can disagree.
      unreadable:
        'The accelerator list could not be read, so only Automatic is offered. The engine runtime row above says why.',
      // A stored accelerator the list does not offer, or any before the list
      // has been read. The id is data, so it is not translated.
      saved: '{id} (saved)',
    },
    // Which modifier is held while clicking the page to set where Clone / heal
    // reads from. The *caps* are not here: `⌥` and `Alt` are the keys' own
    // names, printed on the hardware, and `src/lib/shortcuts.js` supplies them
    // per platform the way it supplies every other keycap. What is here is the
    // spelt-out name, which is what a screen reader says and what the tooltip
    // shows - and it names both keyboards, because one row is read on both.
    cloneSource: {
      label: 'Clone / heal source',
      name: {
        alt: 'Option, or Alt',
        meta: 'Command, or the Windows key',
        control: 'Control',
        shift: 'Shift',
      },
    },
    // Settings > Cloud. The tab's id stays `inference` (the modal spec and the
    // tests name it); what it says is Cloud.
    inference: {
      status: {
        off: 'Cloud GPU is off. Nothing from your pages is sent.',
        checking: 'Checking…',
        ready: 'Ready on “{name}”.',
        attention: 'Needs attention: {reasonKey}',
      },
      reason: {
        none: 'no cloud GPU is set up yet.',
        noTarget: 'no endpoint is chosen as the default.',
        noSecret: 'the default endpoint has no access token on this computer.',
        secretLocked: 'your system password store is locked or blocked this app. Unlock it or allow access, then reopen Settings. Your setup is still saved.',
        unknown: 'the cloud settings could not be read.',
      },
      permission: {
        label: 'Use a cloud GPU',
        description: 'Off keeps every page on this computer. On, each cloud run asks first.',
        off: 'Off',
        on: 'On',
      },
      setup: {
        action: 'Set up with Modal or Beam',
        description: 'In your own account, billed by the provider. You approve the plan first.',
      },
      provider: {
        label: 'Provider',
        modal: 'Modal',
        beam: 'Beam',
        beamPaused: 'Beam (paused)',
      },
      endpoints: {
        title: 'Endpoints',
        empty: 'No cloud endpoints yet.',
        loadFailed: 'The cloud settings could not be read.',
        retry: 'Try again',
        useDefault: 'Use {name} by default',
        // Shown above two or more endpoints: choosing one is the switch.
        switchNote: 'Cloud runs use the default. Each endpoint keeps its own access token, so switching asks for no key.',
        meta: '{providerKey} · {host}',
        // Where a Modal endpoint runs, read from its address.
        account: '{providerKey} account {account}',
        default: 'Default',
        test: 'Test',
        update: 'Update',
        paused: 'Paused',
        testing: 'Testing…',
        remove: 'Remove',
      },
      token: {
        saved: 'Access token saved',
        missing: 'No access token',
        unknown: 'Token not checked',
        add: 'Add token',
        replace: 'Replace token',
        save: 'Save token',
        saving: 'Saving…',
        modalTokenId: 'Token ID',
        modalTokenSecret: 'Token secret',
        beamToken: 'Beam API key',
        saveFailed: 'The token could not be saved in this computer’s keychain. Enter it again to retry.',
      },
      // A name of the person's own, so endpoints in different accounts can be told apart.
      rename: {
        action: 'Rename',
        label: 'Name for {name}',
        save: 'Save name',
        saving: 'Saving…',
        failed: 'The name could not be saved. Try again.',
      },
      // A connection check's answer, and the one setup runs at its end.
      health: {
        reachable: 'The endpoint is reachable.',
        httpError: 'The endpoint answered with an error.',
        unauthorized: 'The endpoint refused the access token.',
        credentialMissing: 'No access token is saved for this endpoint.',
        configuration: 'The endpoint settings are not valid.',
        unreachable: 'The endpoint could not be reached.',
        unknown: 'The endpoint could not be checked.',
        latency: '{latency} ms',
      },
      remove: {
        confirm: 'Remove “{name}” from this computer?',
        keepNote: 'The endpoint and its storage stay in your {providerKey} account until you delete them there.',
        alsoDelete: 'Also delete the cloud resources',
        deleteNote:
          'Next you see what will be deleted from your {providerKey} account, including the model weights, and enter your key to confirm.',
        review: 'Review what is deleted',
        confirmButton: 'Remove',
        failed: 'The endpoint could not be removed. Try again.',
      },
      logs: {
        title: 'Cloud logs ({count})',
        empty: 'No interrupted cloud renders.',
        dismissAll: 'Dismiss all',
      },
      recovery: {
        title: 'Needs attention',
        unfinished: 'A setup in your {providerKey} account did not finish ({id}).',
        forgetNote:
          'Resume picks up where it stopped, and can clean up from there. Forget only clears this reminder: anything already created stays in your account.',
        resume: 'Resume',
        forget: 'Forget',
        noToken: '“{name}” has no access token on this computer.',
        newToken: 'Get a new token',
        outdated:
          '“{name}” runs cloud code from an older version of Manga Cleaner, so GPU status and Stop are not available. Updating sends this version’s code to the same setup; your models and settings stay.',
        update: 'Update cloud code',
        olderCode:
          '“{name}” runs older cloud code than this version of Manga Cleaner. Updating sends this version’s code to the same setup; your models and settings stay.',
        attempt: 'A cloud render for page {page} was interrupted. {reasonKey}',
        attemptNoPage: 'A cloud render was interrupted. {reasonKey}',
        dismiss: 'Dismiss',
        reason: {
          ambiguous: 'It is not known whether the provider ran it, so it was not sent again.',
          stale: 'The region changed after it was sent, so the result was not applied.',
          failed: 'It did not finish. Run it again if you still want it.',
        },
      },
      connect: {
        title: 'Connect an existing endpoint',
        description:
          'For an endpoint you deployed yourself. Enter its HTTPS address and the access token it expects: a proxy auth token for Modal, your API key for Beam.',
        name: 'Name',
        endpoint: 'Endpoint URL',
        connect: 'Connect',
        connecting: 'Connecting…',
        connected: 'Connected and saved: {name}.',
        savedUnchecked: 'Saved {name}. {reasonKey}',
      },
      endpoint: {
        placeholder: 'https://…',
      },
      error: {
        invalidName: 'Enter a name of 1 to 128 characters.',
        invalidUrl: 'Enter an HTTPS address on a public host name, with no user name, query or fragment.',
        tokenRequired: 'Enter the access token.',
        tokenSaveFailed: 'The token could not be saved, so the endpoint was not added. Try again.',
        saveFailed: 'The cloud settings could not be saved. Nothing was changed.',
      },
    },
    // The sidebar's sections. Models' two groups, Detection and Cleaning,
    // take their names from `pipelines.*`, which onboarding shares.
    section: {
      general: 'General',
      models: 'Models',
      cloud: 'Cloud',
      performance: 'Performance',
      shortcuts: 'Shortcuts',
      about: 'About',
      denoise: 'Denoise',
    },
    tabs: {
      label: 'Settings sections',
    },
    // Settings > Denoise: where page denoise runs, the preset, the local
    // model and its measured time.
    denoise: {
      intro: 'Denoise removes scan grain and JPEG noise from whole pages and saves them as new files. Run it from a chapter\'s menu.',
      where: 'Where to run',
      preset: 'Preset',
      localModel: 'Local model',
      localModelNote: 'Needed to denoise on this computer.',
      localTime: 'Time on this computer',
      localTimeNote: 'Denoises one sample page with the local model to see how long a page takes here.',
      offNote: 'Denoise is off. Pick where to run it to choose a preset.',
    },
  },

  /* ================================================================== */
  /* about - the GPL-3.0 obligations                                      */
  /* ================================================================== */
  about: {
    fact: {
      version: 'Version',
      licence: 'Licence',
      source: 'Source',
      detector: 'Detection',
      engines: 'Inpainting',
      cloud: 'Cloud',
      runtime: 'Runtime',
    },
    offer: {
      // The GPL-3.0 written offer. Not decoration: §6 of the licence requires
      // it wherever the corresponding source is not shipped alongside.
      written:
        'Manga Cleaner is free software under the GNU General Public License, version 3 only. The complete corresponding source is available at the address above, and will be supplied on physical media on request for no more than the cost of distribution.',
    },
    note: {
      // Names the providers, as the consent dialog names the endpoint. About is
      // where a user reads about the cloud when they are *not* mid-request, so
      // it is the worse of the two places to leave the provider unnamed.
      cloudTerms:
        'Cloud requests go to the Modal or Beam account you set up, and that provider’s terms govern them, not this licence. The cloud is off until you turn it on, and you confirm the first request in each project. Cloud detection sends whole pages, and cloud cleaning sends a crop around each region.',
    },
  },

  /* ================================================================== */
  /* export - the Export dialog and the overwrite refusal                */
  /* ================================================================== */
  export: {
    meta: {
      chapter: {
        one: '1 page · Ch. {chapter}',
        other: '{count} pages · Ch. {chapter}',
      },
    },
    // JPEG is deliberately absent. Lossy formats belong below the
    // fidelity line, behind an acknowledgement that does not exist here -
    // and a button that always refuses is worse than no
    // button. `notice.export.refusedLossyFormat` is what a caller that asks for
    // one anyway is told.
    format: {
      label: 'Format',
      png: 'PNG',
      tiff: 'TIFF',
      psd: 'PSD',
      cbz: 'CBZ',
    },
    layout: {
      label: 'Layout',
      perPage: 'One file per page',
      stitched: 'One file for the chapter',
    },
    destination: {
      label: 'Destination',
      newFolder: 'New folder',
      sourceFolder: 'Source folder',
      customFolder: 'Another folder',
      pathLabel: 'Export folder',
      pathPlaceholder: '/Users/you/manga/ch01_cleaned',
      chooserTitle: 'Choose a folder to export into',
    },
    masks: {
      label: 'Masks',
      flattened: 'Flattened',
      separateLayer: 'Separate layer',
    },
    note: {
      actualFormats: 'Output files: {formats}.',
      // Pages narrower than the strip get columns beside them
      // that no source file has a pixel for, and an interface offering
      // stitched output states the count *before* it runs - the same standard
      // every other unasked-for change is held to.
      gutter: {
        zero: 'Every page is the full width of the chapter, so the single file invents no pixels.',
        one: '1 pixel of paper white will be written beside the narrower pages. No source file has a pixel there.',
        other:
          '{count} pixels of paper white will be written beside the narrower pages. No source file has a pixel there.',
      },
      flagged: {
        zero: 'Nothing is flagged for review.',
        one: '1 region is still flagged for review. It exports as it is now.',
        other: '{count} regions are still flagged for review. They export as they are now.',
      },
    },
    state: {
      exporting: 'Exporting…',
      planning: 'Checking output formats and color metadata…',
    },
    action: {
      export: 'Export {format}',
      chooseAnotherFolder: 'Choose another folder…',
    },
    refusal: {
      body: 'Export would replace the files in {path}. Cleaned pages must be written somewhere else. Choose another folder and the export will run.',
    },
    // Not an error. The UI never silently downgrades, so a format
    // that cannot carry the source's colour mode produces a written statement
    // of exactly what will change, before the export runs.
    declared: {
      formatChanged:
        'Page {page}: {requested} will be saved as lossless {used} to preserve {mode} samples.',
      metadataNormalized: 'Page {page}: {field} metadata changes: {reason}.',
      assumedInterpretation: 'Page {page}: display uses {interpretation}; native samples are preserved.',
    },
  },

  /* ================================================================== */
  /* home - library, chapters, and Home's four dialogs                   */
  /* ================================================================== */
  home: {
    section: {
      projects: 'Projects',
      chapters: 'Chapters',
    },
    action: {
      newProject: 'New project',
      newChapter: 'New chapter',
      deleteChapter: 'Delete chapter',
      denoise: 'Denoise…',
      denoiseCleaned: 'Denoise cleaned chapter…',
      replaceDenoised: 'Replace pages with denoised',
      denoiseHistory: 'Denoise history',
      chapterMenu: 'Actions for {name}',
      open: 'Open',
      rename: 'Rename',
      remove: 'Remove',
      retry: 'Try again',
      settings: 'Settings',
      continueClean: 'Continue cleaning',
      backToProjects: 'Back to projects',
      copySourcePath: 'Copy source path',
    },
    menu: {
      project: 'Actions for {name}',
    },
    hint: {
      newChapterNeedsProject: 'Create a project first. A chapter belongs to one.',
    },
    project: {
      meta: {
        select: 'chapters',
        one: '1 chapter · {pages} pages · {modeKey}',
        other: '{chapters} chapters · {pages} pages · {modeKey}',
      },
    },
    card: {
      chapterLine: {
        one: 'Ch. {chapter} · {title}',
        other: 'Ch. {chapter} · {title} · {count} chapters',
      },
      cleaned: '{cleaned} of {total} pages cleaned',
      resumeAt: 'Interrupted in Ch. {chapter}, page {page}',
    },
    chapter: {
      number: 'Ch. {number}',
      pages: {
        one: '1 page',
        other: '{count} pages',
      },
      positions: {
        one: '1 position',
        other: '{count} positions',
      },
      cleaned: '{cleaned} of {total} cleaned',
      // A fact in the row's sub-line once the chapter was denoised.
      denoised: 'Denoised',
      denoisedTaken: 'Denoised, pages replaced',
      needReview: {
        one: '1 needs review',
        other: '{count} need review',
      },
      skipped: {
        one: '1 skipped',
        other: '{count} skipped',
      },
      interrupted: 'Interrupted at page {page}',
    },
    empty: {
      libraryTitle: 'No projects yet',
      chaptersTitle: 'No chapters yet',
    },
    state: {
      loading: 'Loading the library…',
      failedTitle: 'The library could not be read',
      failedBody: 'Nothing has been changed. Try again, or check that the library folder is still where it was.',
      projectMissingTitle: 'That project is no longer in the library',
      projectMissingBody: 'It may have been removed. The pages on disk are untouched either way.',
    },
    newProject: {
      source: 'Source folder',
      sourcePlaceholder: 'Choose a folder…',
      name: 'Project name',
      namePlaceholder: 'Untitled project',
      mode: 'Page mode',
      modeNote:
        'Set once, at creation. It decides how pages are stitched and split in every chapter of this project, and it cannot be changed afterwards.',
      permanent: 'Page mode is permanent.',
      createSingle: 'Create single-page project',
      createLongstrip: 'Create longstrip project',
    },
    newChapter: {
      project: 'Project',
      noProject: 'No project selected',
      source: 'Source folder',
      sourcePlaceholder: 'The project’s folder…',
      // The copy is the point: it is what tells a user the folder is read once
      // and can then be moved or deleted.
      sourceNote:
        'The pages are copied into this app and never written back. You can move or delete the folder afterwards.',
      number: 'Chapter number',
      taken: 'Chapter {number} already exists in this project.',
      title: 'Chapter title',
      defaultTitle: 'Chapter {number}',
      create: 'Add Ch. {number}',
    },
    openProject: {
      filter: 'Filter projects',
      filterPlaceholder: 'Type a project name',
      noMatches: 'No project matches that.',
      meta: {
        select: 'chapters',
        one: '1 chapter · {pages} pages',
        other: '{chapters} chapters · {pages} pages',
      },
    },
    deleteChapter: {
      body: 'Delete Ch. {number} “{name}”? Its cleaning is lost. The scans in {path} stay unless you choose to remove them too.',
      // A chapter with no folder of its own reads the project's folder, which
      // the project's other chapters read too - so the scans cannot be offered
      // here at all, and the copy says which folder it is protecting.
      bodyOwnFolder: 'Delete Ch. {number} “{name}”? Its cleaning is lost. The scans stay: this chapter reads the project’s own folder.',
      keepScans: 'Delete chapter',
      withScans: 'Delete chapter and scans',
    },
    // The chapter's last denoise left a file for every page. Only pages with
    // no cleaning on them change, and the detected masks stay.
    replaceDenoised: {
      body: {
        one: 'Chapter {number}: the page becomes its denoised version. Detected masks stay. Your scan files do not change.',
        other: 'Chapter {number}: {count} pages become their denoised versions. Detected masks stay. Your scan files do not change.',
      },
      bodyKept: {
        one: 'Chapter {number}: 1 page becomes its denoised version. Detected masks stay. Pages cleaned or changed after denoise stay as they are ({kept}). Your scan files do not change.',
        other: 'Chapter {number}: {count} pages become their denoised versions. Detected masks stay. Pages cleaned or changed after denoise stay as they are ({kept}). Your scan files do not change.',
      },
      // Some pages have no denoised file: never denoised, or they failed in
      // the run. They stay raw, and the rest can still be taken.
      bodyMissing: {
        one: 'Chapter {number}: 1 page becomes its denoised version. Pages that were not denoised stay raw ({missing}). Detected masks stay. Your scan files do not change.',
        other: 'Chapter {number}: {count} pages become their denoised versions. Pages that were not denoised stay raw ({missing}). Detected masks stay. Your scan files do not change.',
      },
      bodyMissingKept: {
        one: 'Chapter {number}: 1 page becomes its denoised version. Pages that were not denoised stay raw ({missing}). Pages cleaned or changed after denoise stay as they are ({kept}). Detected masks stay. Your scan files do not change.',
        other: 'Chapter {number}: {count} pages become their denoised versions. Pages that were not denoised stay raw ({missing}). Pages cleaned or changed after denoise stay as they are ({kept}). Detected masks stay. Your scan files do not change.',
      },
      confirm: 'Replace pages',
    },
    // A chapter's denoise runs, as the compare view's run picker lists them:
    // when and which preset, then the facts.
    denoised: {
      failed: 'Could not read the denoise history of chapter {number}.',
      // A run's two lines: when and which preset, then the facts.
      run: '{when} · {preset}',
      presetUnknown: 'Preset not recorded',
      fromRaw: 'From raw pages',
      fromCleaned: 'From cleaned pages',
      pages: {
        one: '1 page',
        other: '{count} pages',
      },
      taken: {
        one: '1 taken as page',
        other: '{count} taken as pages',
      },
      missing: {
        one: '1 file missing',
        other: '{count} files missing',
      },
    },
    rename: {
      label: 'Project name',
    },
    remove: {
      // The scans stay, and since the library keeps its own copy of every page
      // that is now the smaller half of the truth: what goes is a copy the user
      // may have deleted their originals for. Both halves, in that order.
      body: 'Remove {name} from the library? Its cleaning and its copy of the pages go. The scans in {path} stay.',
    },
  },

  /* ================================================================== */
  /* project                                                             */
  /* ================================================================== */
  project: {
    mode: {
      single: 'single page',
      longstrip: 'longstrip',
    },
  },

  /* ================================================================== */
  /* editor - the chrome around the canvas                               */
  /* ================================================================== */
  editor: {
    region: {
      identity: 'Project and chapter',
      windows: 'Panels',
      view: 'View',
      tools: 'Tools',
      viewControls: 'View controls',
      chapterNav: 'Pages and review',
      reviewSet: 'Regions needing review',
      canvas: 'Page',
    },
    identity: {
      chapter: '· Ch. {number}',
    },
    direction: {
      rtl: 'right to left',
      ltr: 'left to right',
    },
    panel: {
      pages: 'Pages',
      // The window's own heading, and the name its move / collapse / close
      // buttons take. The design file titles this window LAYERS; `Layers &
      // review` is the panel toggle's name (`shortcuts.panel.masks`), where
      // there is room for it. In a 248px header the long form left 43px for
      // the meta, which rendered `no masks` as `no ma…` and `2 flagged` as
      // `2 flag…`.
      layers: 'Layers',
    },
    meta: {
      pagesCleaned: '{done} of {total} cleaned',
      masksApplied: {
        zero: 'no masks',
        one: '1 mask',
        other: '{count} masks',
      },
      masksFlagged: {
        zero: 'nothing flagged',
        one: '1 flagged',
        other: '{count} flagged',
      },
      detected: {
        one: '1 detected',
        other: '{count} detected',
      },
    },
    action: {
      home: 'Library',
      settings: 'Settings',
      export: 'Export',
      renameProject: 'Rename project',
      pagesWindow: 'Pages',
      layersWindow: 'Layers & review',
      maskOverlay: 'Mask overlay',
      originalHold: 'Hold to show the original',
      originalPin: 'Pin the original',
      wipe: 'Wipe between original and cleaned',
      zoomIn: 'Zoom in',
      zoomOut: 'Zoom out',
      zoomFit: 'Fit the page',
      prevReview: 'Previous region needing review',
      nextReview: 'Next region needing review',
      undo: 'Nothing to undo',
      redo: 'Nothing to redo',
      undoCommand: 'Undo {commandKey}',
      redoCommand: 'Redo {commandKey}',
      // The run button names the step and the scope (docs/detect-clean.md).
      // One entry per pair, so a language can order the two its own way.
      run: {
        autoPage: 'Detect & clean page',
        autoChapter: 'Detect & clean chapter',
        autoProject: 'Detect & clean project',
        detectPage: 'Detect page',
        detectChapter: 'Detect chapter',
        detectProject: 'Detect project',
        cleanPage: 'Clean page',
        cleanChapter: 'Clean chapter',
        cleanProject: 'Clean project',
      },
      textShapeReview: 'Text-shaped review',
      cancelRun: 'Cancel run',
    },
    readout: {
      page: 'Page {index} of {total}',
      review: 'Region {index} of {total}',
      reviewPending: {
        select: 'total',
        zero: 'Nothing to review',
        one: '1 to review',
        other: '{total} to review',
      },
      zoomActual: '{percent}% · click for 1:1',
      zoomActualFromFit: 'Fitted · click for 1:1',
    },
    zoom: {
      fit: 'Fit',
    },
    // Four characters at most: the readout box is 30px wide. The full words
    // are spoken through the slider's aria-valuetext, not printed here.
    wipe: {
      clean: 'clean',
      original: 'orig',
    },
    window: {
      titleBar: '{windowName} window',
      move: 'Move {windowName}',
      resize: 'Resize {windowName}',
      collapse: 'Collapse {windowName}',
      expand: 'Expand {windowName}',
      close: 'Close {windowName}',
    },
    run: {
      progress: '{done} of {total}',
    },
    status: {
      // Beside the Text cleanup panel's progress and its Cancel while a run is
      // going, in a live region. There is no idle counterpart: the two "click
      // a region to apply X" lines the tool window used to carry were a footer
      // explaining a tool the user had just chosen from the rail. A Detect run
      // changes no pixel, so it says so rather than claiming to clean.
      cleaning: 'Cleaning page {page}',
      detecting: 'Detecting page {page}',
    },
    state: {
      opening: 'Opening the chapter…',
      noPages: 'This chapter has no pages.',
      cloudBlocked: 'Cloud GPU is off in Settings.',
      // A detection model the run needs is missing and which one is not known
      // yet (the model list has not been read). Disabled with a sentence
      // rather than hidden: an engine option has four alternatives beside it
      // and this button has none, so a tool that quietly lost its only action
      // would read as a broken window rather than as a missing download.
      modelsMissing: 'A detection model is missing on this computer. Download it in Settings › Models.',
      // The selected detection models this computer does not have, by name.
      // Only detection is named: a clean opens no detector, so LaMa Manga
      // being here does not help. The cloud form is for models that have a
      // cloud version, with Detect on set to This computer.
      detectModelsMissing: 'Not on this computer: {models}. Download in Settings › Models.',
      detectModelsCloud: 'Not on this computer: {models}. Detect on your cloud GPU, or download in Settings › Models.',
      runtimeMissing: 'No engine runtime found. Download it in Settings › Performance.',
    },
  },

  /* ================================================================== */
  /* canvas - the page surface                                           */
  /* ================================================================== */
  canvas: {
    region: {
      name: '{titleKey} · {statusKey}',
      nameFlagged: '{titleKey} · {statusKey}. {reasonKey}.',
    },
    action: {
      clearSelection: 'Page: activate to clear the selection',
      draw: 'Drawing surface: activate to open a draft region',
      drawActive: 'Draft region: arrows move, Shift and arrows resize, Enter commits, Escape abandons',
    },
    draft: {
      size: '{w} × {h}%',
    },
    command: {
      // One label for four applications (a region click, a content-aware fill,
      // an AI snap onto an existing region, a cloud fill). It says what the
      // undo entry undoes without claiming which tool made it - the tool is
      // already named in the Layers row this entry came from.
      applyTool: 'the tool applied to a region',
      drawMask: 'a mask drawn by hand',
    },
  },

  /* ================================================================== */
  /* pages - the Pages list                                              */
  /* ================================================================== */
  pages: {
    label: {
      page: 'p. {number}',
      position: 'pos {number}',
    },
    status: {
      unclean: 'not cleaned',
      cleaning: 'cleaning now',
      cleaned: 'cleaned',
      cleanedNeedsReview: 'cleaned, needs review',
      skipped: 'skipped',
      // Text found and stored by Detect, nothing cleaned yet.
      detected: 'text found, not cleaned yet',
      held: 'areas held for your choice',
    },
    row: {
      // `count` is the number of regions still needing review, and it is 0 on
      // most rows - which is why the zero form drops the clause entirely
      // rather than saying "0 regions need review".
      name: {
        zero: '{label} · {statusKey}, {cleaned} of {total} regions',
        one: '{label} · {statusKey}, {cleaned} of {total} regions, 1 needs review',
        other: '{label} · {statusKey}, {cleaned} of {total} regions, {count} need review',
      },
      skipped: '{label} · skipped: {reasonKey}',
    },
  },

  /* ================================================================== */
  /* paging                                                              */
  /* ================================================================== */
  paging: {
    action: {
      prev: 'Previous page',
      next: 'Next page',
    },
  },

  /* ================================================================== */
  /* progress - the Home rollups                                         */
  /* ================================================================== */
  progress: {
    status: {
      notStarted: 'Not started',
      inProgress: 'In progress',
      review: 'Needs review',
      completed: 'Completed',
    },
  },

  /* ================================================================== */
  /* review - why a region is flagged                                    */
  /* ================================================================== */
  review: {
    reason: {
      // Read from the cloud attempt journal, cloud on or off. Only the first
      // says a committed result went missing: the journal kept the result and
      // its evidence; the area needs cleaning again or the saved result
      // restored. `cloud.recovery.repairNeeded` says the same on the notice.
      repairNeeded: 'A saved cloud result for this area is missing or no longer matches the page, so it needs repair',
      // A result arrived from the cloud and was never put on the page.
      cloudResultNotApplied: 'A cloud result for this area arrived but was never applied',
      // A committed cloud result whose chapter or page could not be read to
      // check that it is still there.
      cloudResultUnchecked: 'A saved cloud result for this area could not be checked',
      inputChanged: 'An earlier edit changed what this layer read',
      inputUnknown: 'An earlier edit may have changed what this layer read',
      fittingReconstructed: 'Fitting failed, so a model reconstructed the area',
      unusuallyLarge: 'Unusually large for this page',
      // The kind of failure. The decliner's own `decline.reason.*` key is
      // shown beside it as the cause (`review.fact.cause`).
      declined: 'Cleaning failed, so the original text was left alone',
      gateSkippedLowConfidence: 'The script gate was not confident enough to clean it',
      gateSkippedOutsideBubble: 'Text outside a speech bubble',
      // Not a failure of the gate - the opposite. It read the script, the
      // script was not one the cleaner targets, and leaving Latin text alone is
      // the product.
      // Distinct from `low-confidence` because
      // reporting a confident refusal as an uncertain one is backwards.
      gateSkippedNotJapanese: 'Not Chinese, Japanese, or Korean; left as it was',
      gateSkippedNotText: 'The text reader found no lettering here; left as it was',
      languageSkipped: 'Held because this language was skipped in the run settings',
      outsideLanguageUnverified: 'Held because outside-bubble text could not be checked against the selected languages',
      maskNeedsCorrection: 'The text-shaped mask needs correction before it can be applied',
      checkGeneratedTexture: 'Check generated texture',
      // Grouping reasons (`cleaner_core::text_groups::ReviewReason`). Each is
      // a fact about the detection evidence, never a confidence score.
      unassignedMask: 'Lettering no text box claims; held, not cleaned automatically',
      isolatedMask: 'A lone mark with no lettering beside it; held in case it is artwork',
      crossesBalloon: 'The lettering mask reaches outside its speech bubble',
      maskMissingUnderBox: 'A text box with no lettering mask under it',
      // A stored reason from another version of the app that this one has no
      // meaning for. Kept flagged rather than cleared (`library.rs#review_flags`).
      unrecognized: 'Flagged by another version of the app for a reason this version does not recognise',
      // Legacy, and no longer shown: an accepted cloud render is not flagged,
      // and old jobs that stored this are migrated on load. The native side
      // still names the key to recognise it.
      cloudAccepted: 'A cloud request was accepted, so check it against the page',
      cloudRejectedSafetyFilter: 'the provider’s safety filter refused it',
      cloudRejectedTransportError: 'the request never completed',
      cloudRejectedParameterTest: 'it failed the parameter test',
      cloudRejectedResidualTest: 'it failed the residual test',
      cloudRejectedStructural: 'it failed the structural test',
    },
    // The region state a held text-group candidate is in
    // (`model/review.js#regionState`). The other states keep their
    // `masks.status.*` words.
    state: {
      candidate: 'Held for your choice',
    },
    // Lettering grouping held back instead of cleaning: no text box claimed
    // it, or it is a lone mark. Not a failure and not a problem, so it is
    // never counted as either; the user cleans it or dismisses it.
    candidate: {
      title: 'Possible text or artwork',
      heldBecause: 'Held because',
      clean: 'Clean it',
      cleanHint: 'Clean this area. No text box claimed it, so check it is lettering and not artwork first.',
    },
    fact: {
      // Beside a failure's kind: what the decliner said.
      cause: 'Cause',
      detail: 'What to check',
    },
    detail: {
      generatedTexture: 'Generated texture can differ from the source. Check screentone, gradients and line art.',
    },
    meta: {
      // The Layers header, when the page holds candidates: `{base}` is the
      // header's own count, already worded.
      withCandidates: {
        one: '{base} · 1 held',
        other: '{base} · {count} held',
      },
    },
    page: {
      // Appended to a Pages row's accessible name.
      candidates: {
        one: '1 area held for your choice',
        other: '{count} areas held for your choice',
      },
    },
  },

  /* ================================================================== */
  /* decline / input - terminal outcomes the pipeline reports            */
  /* ================================================================== */
  decline: {
    reason: {
      qualityMetric: 'no engine met the quality metric',
      // The metric has two halves, and a reader acting on
      // them acts differently: the first says the engine put something on the
      // paper, the second says it put nothing recognisable as that paper. One
      // is a reason to look at the region, the other is a reason to look at the
      // page it was cut from. `qualityMetric` above is the pair of them said at
      // once, which is all the core could say while these two did not exist.
      edgeEnergy: 'every engine left strokes the paper around it does not have',
      histogram: 'no engine matched the tone of the paper around it',
      // Past four model inputs a region is declined
      // rather than tiled, because leaving text is recoverable and a bad fill
      // is not.
      tooLarge: 'the region is larger than the inpainter can cover',
      // Indexed and CMYK sources
      // cannot carry a model rung's output at all.
      unrepresentableMode: 'this file’s colour mode cannot hold an inpainted result',
      paintUnsupportedColor: 'this file’s color description cannot be used for managed painting',
      paintUnsupportedMode: 'this file’s colour mode cannot hold a painted or cloned result',
      unrepresentableDepth: 'this file has one bit per sample, which an inpainted result cannot hold',
      // Rung 3a only, and a narrower fact than `unrepresentableDepth` above:
      // the file's samples are perfectly representable, and it is that one
      // engine which is not built to carry them. The AI redraw helper works in
      // 8 bits per sample (`engines::flux::declines`), so a 16-bit source that
      // the default inpainter handles at full depth cannot go to it. The remedy
      // is to use the default inpainter, which is why this does not read as a
      // property of the file.
      //
      // It said "this fast preview engine" until MI-GAN was removed. The rung
      // is gone; the sentence was still being emitted, by rung 3a, under the
      // departed rung's description.
      depthBeyondEngine: 'the AI redraw helper works at 8 bits, and this file has more',
      // Not a judgement on the region at all - no engine was ever asked about
      // it. Two ways in, and the remedy differs: the engine ceiling was set
      // below the rung the region needed, or this machine has no weights for
      // that rung. Distinct from the four above, each of which is something an
      // engine decided after looking.
      rungUnavailable: 'the engine this region needs was not available',
      // The engine was asked, it started, and then it stopped answering: an
      // ONNX session that built and ran and died under it, which on Windows is
      // usually the graphics driver being reset out from under the run. Said
      // as a fact about the model rather than about the region, because the
      // region is fine and the next attempt on a fresh session may well work.
      // The remedy is not here: it is `notice.run.engineFault`, which names
      // the model and the provider and says what to change. Until this key
      // existed the edit rejected with ONNX Runtime's own string, which no
      // catalogue lookup catches, so the click did nothing visible at all.
      engineFault: 'the model stopped answering',
      // Rung 3a's four. None of them
      // is a fact about the region - they are all facts about the machine or
      // the process, which is what separates them from the six above and why
      // each names a different remedy. There is no key for the sidecar simply
      // not being installed: that is the ordinary state, it is reported as
      // nothing at all, and a rung the user never installed is not a failure
      // the user has to dismiss.
      sidecarMachine:
        'this machine does not have the memory the FLUX sidecar needs for this model',
      // A probe that was made and failed - a sysctl, a /proc read, a
      // GlobalMemoryStatusEx. This used to be every Windows and Linux machine;
      // now both are measured, and what stops them is the row
      // below. A different sentence from the one above because the remedy is
      // different - this is the application's gap, not the machine's.
      sidecarUnknownMachine:
        'this machine’s memory could not be measured, so the FLUX sidecar is not offered here',
      // The *chosen* backend has no render path here. It used to be every
      // Windows and Linux machine, because mflux was the
      // only rendering backend and MLX is Apple Silicon; since the sdnq backend
      // there is no platform without one, so what reaches this is a user who
      // picked mflux on a machine with no MLX. Not a fact about the machine's
      // memory and deliberately not phrased as one.
      sidecarPlatform:
        'this build has no FLUX sidecar for this kind of computer, so the helper is not offered here',
      // Installed, and installed without the chosen engine's Python packages -
      // a virtual environment may hold either backend's. Its own sentence
      // because its own remedy is one command, unlike the row above.
      sidecarBackend:
        'this FLUX sidecar folder does not have the chosen engine installed',
      // The third guard doing its job: the sidecar hit the ceiling
      // it was given and raised, rather than driving the machine into swap.
      sidecarMemory: 'the FLUX sidecar reached the memory it was allowed',
      // The first guard: "adding a backend means adding its bound, or
      // refusing to offer it".
      sidecarUnbounded:
        'this FLUX sidecar cannot say how much memory it needs, so it is not run',
      // Almost absence, and deliberately not silent like absence: this is a
      // user who installed the sidecar and is one download away from the rung.
      sidecarWeights: 'the FLUX sidecar is installed and has no model weights yet',
      // The one entry in this group the core never chooses: the interface does,
      // when a region edit is rejected with something that is not a key at all.
      // The backend answers most failures with one of the reasons above, and it
      // can still reject with a sentence of its own - a disk that would not
      // write, a library's own words - and putting that on screen is how a
      // reader ends up looking at an ONNX Runtime stack trace in a notice. This
      // says the true and useful part instead, and the sentence around it says
      // the region is untouched, which is the half that matters. The raw text
      // is not lost: it goes to the console, where the other unreadable
      // diagnostics go.
      unknown: 'the engine failed for a reason this build does not recognise',
    },
  },
  // Why a file did not make it into a chapter. The
  // rule is that a skip is always *listed*, never silent, so each of these is
  // read by a user deciding whether the file matters.
  input: {
    skipReason: {
      notAnImage: 'it is not an image this reads',
      headerUnreadable: 'its header does not parse',
      // The truncated-scan case §2 singles out: the header parsed and the
      // pixels did not, which is exactly when a partial decode would produce a
      // "cleaned" grey tail.
      partialDecode: 'it does not decode completely',
      unsupportedMode: 'its colour mode has no policy here, and promoting it would change the file',
      duplicate: 'another file has the same contents',
      junk: 'it is a system file',
      // The file was fine and this application's own write of the PNG made
      // from it was not - a full disk, a read-only library. Said separately
      // from `partialDecode` so nobody goes looking at the scan.
      conversionFailed: 'it could not be saved as a PNG',
      // The same failure for a file that needed no converting: the page is a
      // PNG or TIFF already and the library's own copy of it could not be
      // written. Separate from `conversionFailed` so the sentence is not about
      // a conversion that never happened.
      importFailed: 'it could not be copied into the library',
    },
  },

  /* ================================================================== */
  /* diagnostics - why the ONNX Runtime is not usable                    */
  /* ================================================================== */
  // Five states, told apart because their remedies are: download it, clear an
  // extended attribute, re-sign the application, install a Microsoft
  // redistributable, replace the file.
  diagnostics: {
    runtime: {
      missing: 'The ONNX Runtime was not found',
      quarantined: 'The ONNX Runtime is quarantined, so the system refused to load it',
      refused: 'The system refused to load the ONNX Runtime into this application',
      // Windows, and the one of the five that is not about our file at all: the
      // `onnxruntime.dll` we ship imports `MSVCP140.dll`, `MSVCP140_1.dll`,
      // `VCRUNTIME140.dll` and `VCRUNTIME140_1.dll`, which arrive with the
      // Microsoft redistributable and with nothing else. Without it the load
      // fails with Windows error 126, which read as `unloadable` until now -
      // and that sent the reader to download our file again, when our file was
      // never the thing that was wrong. The product is named in full because
      // the full name is what has to be searched for to fix this.
      missingDependency:
        'The Microsoft Visual C++ 2015-2022 Redistributable (x64) is not installed, so the ONNX Runtime cannot load. Install it from Microsoft and start Manga Cleaner again',
      unloadable: 'The ONNX Runtime could not be loaded',
    },
  },

  /* ================================================================== */
  /* accel - which execution provider a model ran on                     */
  /* ================================================================== */
  accel: {
    cpu: 'CPU',
    coreml: 'CoreML',
    directml: 'DirectML',
    cuda: 'CUDA',
    tensorrt: 'TensorRT',
    rocm: 'ROCm',
    openvino: 'OpenVINO',
    webgpu: 'WebGPU',
    xnnpack: 'XNNPACK',
    // Not an execution provider: rung 3a runs in a separate program, so "where
    // is it running" is answered by naming the program rather than a provider
    // inside this one (cleaner_core::registry::Device::sidecar).
    sidecar: 'Helper app',
    declined: {
      unavailable: 'not available in this runtime',
      // A provider without a `DFT` kernel does not fail, it makes
      // the graph split and run slower.
      partitioned: 'it would split this model and run it slower',
      failed: 'it could not build a session for this model',
      // Forcing CoreML costs 8.19 GB on rung 2 and 5.62 GB on
      // rung 3, and this is the one refusal that is about the machine rather
      // than about the run. The figures travel beside the key, on the
      // selection, because a byte count is not translatable.
      memory: 'this machine has less memory than that provider needs for this model',
      // The other absence, and it is a different remedy: the provider is in the
      // downloaded runtime and the parts NVIDIA ships - the CUDA runtime,
      // cuDNN - are not on this machine. `unavailable` above would send someone
      // to re-download the one thing that is already right.
      missingRuntime: 'it needs CUDA and cuDNN installed on this machine',
      // The third absence: the WebGPU plugin is loaded and
      // found no graphics device. A driver rather than a download - on Linux
      // the Vulkan loader, `libvulkan.so.1`, which every GPU driver package
      // carries; on Windows a working graphics driver.
      noDevice: 'it found no graphics device, so install your graphics driver (on Linux, the Vulkan loader)',
    },
    chosen: {
      // The second tier. A guess stated as a guess: the alternative is
      // writing a guess down as a measurement, which is the error the log
      // otherwise records.
      unmeasured: 'chosen without a measurement on this hardware',
    },
  },

  /* ================================================================== */
  /* models - what is loaded right now, in the corner of the editor      */
  /* ================================================================== */
  //
  // The names are what the thing *does for the reader*, not what it is:
  // nobody outside this repository knows what a segmentation model or an
  // inpainter is, and the tab exists so that a person watching their machine
  // slow down can see what is using the memory and stop it. `models.kind.*`
  // matches `cleaner_core::registry::Kind::label_key` key for key.
  models: {
    // Not "loaded models": the word the row is about is memory, which is what
    // the reader came here worried about.
    title: 'Using memory now',
    kind: {
      textDetector: 'Comic Text Detector (CTD)',
      balloonDetector: 'Ogkalu comic text & bubble detector (Small)',
      fullRt: 'Ogkalu comic text & bubble detector (Full)',
      samTs: 'SAM-TS-L lettering mask',
      scriptGate: 'ogkalu Image Script Identification',
      // Its labels ship as a second file and the two must match: a gate with
      // the wrong labels is not a gate. Named separately because Settings
      // lists one row per *file*, and a row with no name is a row nobody can
      // decide about.
      scriptGateLabels: 'Image Script Identification labels',
      // The gate's rescue reader. Named for what it does rather
      // than for what it is: it reads the Japanese in a balloon the language
      // checker could not make out, so that an ordinary line of dialogue is
      // cleaned instead of landing in review. Three files, three rows in
      // Settings, one name between them plus two that say which part - the
      // same shape the language checker and its labels have.
      ocr: 'Manga OCR',
      ocrDecoder: 'Manga OCR decoder',
      pageDenoise: 'waifu2x page denoise',
      pageDenoiseSeams: 'waifu2x tile blending filter',
      ocrVocab: 'Manga OCR vocabulary',
      hayai: 'Hayai OCR text reader',
      hayaiVision: 'Hayai OCR image encoder',
      hayaiDecoder: 'Hayai OCR decoder',
      hayaiTokenizer: 'Hayai OCR tokenizer',
      // Rung 2. "Redraw" is the word the tools already use for what an
      // inpainter does to the paper under the text.
      inpainter: 'LaMa Manga',
      // Rung 3a, which is a separate program on the machine and is the only
      // row that can be holding several gigabytes.
      sidecar: 'FLUX',
    },
    value: {
      // `{bytes:memory}` - see `FORMATS.memory` in src/lib/i18n/index.js.
      size: '{bytes:memory}',
      // The same figure when the row's `basis` is not a measurement of a
      // running session - the size of the `.onnx` on disk for four of the six
      // rows, which is a *floor*: a session is the weights plus the execution
      // provider's arena. The tilde is the
      // whole of the correction, and it is the honest one: inventing a
      // per-provider coefficient would dress an estimate up as the measurement
      // this row does not have.
      sizeApprox: '~{bytes:memory}',
      // The row while the request is in flight. Unloading happens between
      // regions, so a click during a busy page waits a moment, and saying so
      // is better than a button that appears to have done nothing.
      unloading: 'Freeing…',
    },
    action: {
      // A per-row button, so the name of what is being closed is in the label
      // rather than only in the row above it.
      unload: 'Free the memory {name} is using',
    },
    hint: {
    },
  },

  /* ================================================================== */
  /* onboarding - the setup shown on a first launch                      */
  /* ================================================================== */
  // Six short steps, one choice each, every one of them skippable and every
  // choice also in Settings, which is why so much of this names Settings: it
  // is the way back for a user who skips, declines, or is interrupted. The
  // sizes are real byte counts and the copy says what a thing does rather
  // than what it is, for `models.kind.*`'s reason. Settings' own names are
  // reused where a row is the same setting, so one setting has one name.
  // The pipelines a page goes through (`model/pipelines.js`). Engine names are
  // product names and are not here; what is here is what each one does.
  pipelines: {
    detection: 'Detection',
    cleaning: 'Cleaning',
    clean: 'Clean',
    // Wherever the detection models are shown while Text cleanup detects on
    // the cloud GPU: that run's combination is fixed (`pipelines.js#runDetection`).
    cloudCombo: 'Cloud GPU uses all four models for the best result: CTD, Ogkalu Full, SAM-TS-L and the Hayai OCR text reader. CTD and Hayai OCR run on this computer.',
    language: {
      ja: 'Japanese',
      zh: 'Chinese',
      ko: 'Korean',
    },
    detector: {
      ctdRtdetr: 'CTD finds text, the Ogkalu detector finds bubbles',
    },
    // Said as a run starts with detection on the cloud GPU, once per selected
    // model that has no cloud version, so a cloud run is never quietly mixed.
    run: {
    },
    workflow: {
      ocrRescue: 'Use the Hayai OCR text reader (optional)',
      ocrRescueDescription: 'Reads each region on this computer before cleaning. It holds art that is not text, and confirms unclear Japanese, Chinese and Korean script. Works with both text policies.',
      ratingsNote: 'Engine ratings are provisional estimates, not measurements from one shared benchmark.',
    },
    cleaner: {
      lamaManga: 'Fast redraw tuned for manga tone',
      bigLama: 'Larger LaMa for wide areas',
      flux: 'AI redraw through the FLUX helper',
      fluxCloud: 'AI redraw on your cloud GPU',
      qwen: 'Strongest AI redraw on your cloud GPU, best on sound effects over art',
    },
    column: {
      engine: 'Engine',
      efficiency: 'Efficiency',
      light: 'Lightweight',
      size: 'Size',
    },
    rating: {
      efficiency: 'Efficiency: {value} of 5',
      light: 'Lightweight: {value} of 5',
    },
    status: {
      installed: 'Installed',
      soon: 'Soon',
      // A FLUX model the helper lists: nothing to download here.
      found: 'Via helper',
      needsHelper: 'Needs helper',
      cloudSetup: 'Available in cloud setup',
    },
    skip: 'Skip',
    detectorFor: 'Clean {language}',
  },
  onboarding: {
    title: 'Set up Manga Cleaner',
    stepOf: 'Step {current} of {total}',
    progressLabel: 'Setup progress',
    saveFailed: 'This setting could not be saved. Try again, or change it later in Settings.',
    action: {
      start: 'Set up the app',
      next: 'Continue',
      back: 'Back',
      skip: 'Skip setup',
      skipStep: 'Skip',
      notNow: 'Not now',
    },
    welcome: {
      heading: 'Welcome to Manga Cleaner',
      body: 'An open source project that cuts the time spent cleaning and redrawing comic raws.',
      source: 'GitHub',
    },
    theme: {
      heading: 'Pick a theme',
    },
    token: {
      heading: 'Hugging Face key',
      body: 'Optional. A key makes model downloads faster and less likely to be rate limited.',
      label: 'API key',
      placeholder: 'hf_…',
      saved: 'A key is saved.',
      save: 'Save and continue',
      failed: 'The key could not be saved. Try again, or skip.',
    },
    background: {
      heading: 'Keep running in the background?',
      keep: 'Keep running',
      keepNote: 'The close button hides the window. Downloads and cleaning continue. Quit from the tray icon.',
      quit: 'Quit on close',
      quitNote: 'The close button quits the app. Downloads and cleaning stop. Unfinished downloads keep their progress for next time.',
    },
    detection: {
      heading: 'Detection',
      body: 'Choose where detection runs and which models it uses, then which source languages to clean. Skip a language to leave it untouched.',
      // Under the fixed four while setup's Detect on is Cloud GPU, and under
      // Detect on while Cloud GPU cannot be offered yet (`DetectOnChoice`).
      cloudNow: 'Setup downloads only what runs on this computer: CTD and the Hayai OCR text reader. Your own choice of models is kept for This computer.',
      cloudLater: 'Cloud GPU can be chosen once a cloud GPU is set up, in the Cloud step or later in Settings > Cloud.',
    },
    cleaning: {
      heading: 'Cleaning',
      body: 'Choose the models that redraw the art under removed text.',
      helper: 'FLUX helper (optional)',
      helperNote: 'Only for AI redraw on this computer. Skip it to clean with LaMa Manga, or run FLUX on your cloud GPU instead. Choose its folder if it is installed.',
      helperMissing: 'No helper was found in that folder.',
    },
    cloud: {
      heading: 'Cloud GPU',
      body: 'Optional: run AI redraw on your own Modal GPU. You pay Modal directly.',
      consent: 'Nothing is sent to it until you confirm, once for each project.',
      setUp: 'Set up now',
      update: 'Update cloud setup',
      on: 'Cloud cleaning is already on. To add page denoise or send this version’s cloud code, choose Update cloud setup.',
      withDenoise: 'Setup adds page denoise, as you chose in the last step.',
      addDenoise: 'You chose page denoise on the cloud GPU, and your setup does not have it yet. Choose Update cloud setup to add it.',
      ready: '{name} is set up, and cloud cleaning is on.',
      readyUnnamed: 'Your cloud GPU is set up, and cloud cleaning is on.',
      unchecked: '{name} is set up, but it did not answer its first check. Test it and turn cloud cleaning on in Settings > Cloud.',
      uncheckedUnnamed:
        'Your cloud GPU is set up, but it did not answer its first check. Test it and turn cloud cleaning on in Settings > Cloud.',
      saveFailed: 'Your cloud GPU is set up, but cloud cleaning could not be turned on. Turn it on in Settings.',
    },
    community: {
      heading: 'Join the community',
      intro: 'Stay close to the people building Manga Cleaner.',
      bugs: 'Report a bug',
      bugsDetail: 'Share what went wrong so we can fix it.',
      updates: 'Get updates',
      updatesDetail: 'See what is new and what is coming next.',
      ideas: 'Suggest improvements',
      ideasDetail: 'Tell us what would make your workflow better.',
      join: 'Join our Discord server',
      external: 'Opens in your browser',
    },
    dependencies: {
      heading: 'Dependencies',
      body: 'Checked on {platform}.',
      runtime: 'Engine runtime',
      acceleration: 'Graphics acceleration',
      cpuOnly: 'CPU only',
      afterRuntime: 'Checked after the runtime installs',
      unreadable: 'Could not be read',
      unavailable: 'Not published for this computer. Install it by hand to run local models.',
      needs: 'Needs {items}, installed by you',
      helper: 'FLUX helper (optional)',
      helperFound: 'Found',
      helperMissing: 'Not installed. Not needed to clean.',
      installed: 'Installed',
      toDownload: '{bytes:memory}',
      total: '{count} to download, {bytes:memory}',
      nothing: 'Nothing to download',
      start: 'Download',
      platform: {
        macArm: 'macOS on Apple silicon',
        macIntel: 'macOS on Intel',
        windows: 'Windows',
        windowsArm: 'Windows on ARM',
        linux: 'Linux',
        linuxArm: 'Linux on ARM',
        // Not `other`: that name is a plural category to the catalogue.
        unknown: 'this computer',
      },
    },
    downloads: {
      heading: 'Downloads',
      status: {
        waiting: 'Queued',
        active: '{percent}%',
        starting: 'Starting',
        paused: 'Paused',
        done: 'Done',
        failed: 'Failed',
      },
      pause: 'Pause {name}',
      resume: 'Resume {name}',
      retry: 'Retry {name}',
      pauseAll: 'Pause all',
      resumeAll: 'Resume all',
      later: 'Continue in background',
      close: 'Finish later',
      empty: 'Nothing to download.',
    },
    denoise: {
      cloudNext: 'The next step sets up your cloud GPU with page denoise.',
      cloudAdd: 'Your cloud GPU has no page denoise yet. The next step adds it: choose Update cloud setup.',
      heading: 'Page denoise',
      body: 'Denoise removes scan grain and JPEG noise from whole pages. Choose where it runs. You can change this later in Settings.',
      local: 'This computer',
      localNote: 'Downloads one model. Slower, and nothing leaves this computer.',
      cloud: 'Cloud GPU',
      cloudNote: 'Uses your cloud GPU. Faster, with more presets. Asks before sending pages.',
      off: 'Don\'t use denoise',
      offNote: 'Nothing is downloaded. Turn it on later in Settings.',
      presets: 'Preset',
      download: 'The local model is added to your downloads: {bytes:memory}.',
      installed: 'The local model is already installed.',
      measureLater: 'The time per page on this computer is measured after the download, in Settings > Denoise.',
    },
    // Settings > General, for anyone who skipped a step or wants the tour.
    replay: {
      label: 'Setup',
      description: 'Walk through the theme, models, cloud GPU, community and downloads again.',
      action: 'Run setup again',
      failed: 'Setup could not open because the model list could not be read. Try again.',
    },
  },

  /* ================================================================== */
  /* denoise - page denoise: presets, times, the chapter dialog          */
  /* ================================================================== */
  denoise: {
    title: 'Denoise pages',
    titleCleaned: 'Denoise cleaned pages',
    // One run's pages against the raw pages they were made from, a page at a
    // time, with a wipe between them.
    compare: {
      title: 'Compare denoised pages',
      meta: 'Chapter {number}',
      run: 'Run',
      page: 'Page {page} of {count}',
      previous: 'Previous page',
      next: 'Next page',
      fullscreen: 'Full window',
      wipe: 'Denoised',
      wipeValue: '{value}% denoised',
      denoised: 'Denoised',
      raw: 'Raw',
      folder: 'Saved in {folder}',
      taken: 'This file is now the page. Raw is the scan it replaced.',
      missing: 'The file this run wrote for page {page} is gone. It was saved in {folder}.',
      rawMissing: 'The raw page this file was made from could not be read.',
      failed: 'Could not load page {page}.',
      loading: 'Loading page {page}…',
      empty: 'This run has no pages left in the chapter.',
    },
    meta: {
      one: 'Chapter {number} · 1 page',
      other: 'Chapter {number} · {count} pages',
    },
    target: {
      label: 'Where to run',
      local: 'This computer',
      cloud: 'Cloud GPU',
      off: 'Off',
      profile: 'Cloud profile',
      cloudUnavailable: 'Cloud GPU needs a cloud GPU set up and cloud use turned on in Settings > Cloud.',
      cloudNotSetUp: 'Your cloud GPU was set up without page denoise. In Settings > Cloud, run setup again with Page denoise on.',
      cloudUnchecked: 'Could not reach your cloud GPU to check for page denoise. A cloud run may fail.',
    },
    // Presets: a model name, then what it does. Model names are product
    // names and stay as they are in every language.
    preset: {
      label: 'Preset',
      mangajanai2x: {
        name: 'MangaJaNai 2x',
        note: 'Manga-trained sharpening for soft or small scans.',
      },
      mangajanai4x: {
        name: 'MangaJaNai 4x',
        note: 'Stronger manga-trained sharpening for very small scans. Slow.',
      },
      waifu2xScan: {
        name: 'waifu2x scan, medium noise',
        note: 'Removes scan grain and sharpens. Also runs on this computer.',
      },
      realcugan2x: {
        name: 'Real-CUGAN 2x conservative',
        note: 'Light cleanup that keeps the original look. The fastest.',
      },
      realcugan3x: {
        name: 'Real-CUGAN 3x conservative',
        note: 'Keeps the original look, with more sharpening.',
      },
      realcugan3xStrong: {
        name: 'Real-CUGAN 3x strong denoise',
        note: 'Removes heavy grain and JPEG noise. Can soften fine screentone.',
      },
    },
    credit: '{name} by {author}, {license}',
    duration: {
      seconds: '{value} s',
      minutes: '{value} min',
      hours: '{hours} h {minutes} min',
    },
    time: {
      cloud: '{duration} per page',
      cloudWhere: 'on a cloud {gpu} GPU',
      local: '{duration} per page',
      localWhere: 'on this computer',
      notMeasured: 'Not measured yet',
      measure: 'Measure',
      remeasure: 'Measure again',
      measuring: 'Measuring one page, about 30 to 60 seconds',
      measureFailed: 'Measuring failed. Check that the local model is downloaded, then try again.',
      needsModel: 'Download the local model to measure it.',
    },
    model: {
      notInstalled: 'Not installed · {bytes:memory}',
      unavailable: 'This version has no local denoise model to download yet.',
      openSettings: 'Open Settings',
    },
    estimate: {
      label: 'Estimated time',
      value: 'About {duration}',
      pages: 'GPU time for these pages at their size. Upload and GPU start-up come on top.',
      reference: 'Page sizes are unknown, so this assumes {width} x {height} pages. Upload and GPU start-up come on top.',
      measured: 'From the time measured on this computer.',
    },
    out: {
      label: 'Save to',
      placeholder: 'A folder for the denoised pages',
      chooserTitle: 'Choose where to save the denoised pages',
      note: 'Each page is saved as a PNG. The chapter\'s scans are not changed.',
    },
    setup: {
      heading: 'Denoise is off',
      body: 'Choose where to run it. You can change this later in Settings > Denoise.',
    },
    action: {
      run: {
        one: 'Denoise 1 page',
        other: 'Denoise {count} pages',
      },
      review: 'Review cost',
      stop: 'Stop',
      back: 'Back',
      retry: 'Try again',
      copyPath: 'Copy folder path',
    },
    busy: {
      local: 'Denoising on this computer. This can take a while; keep the app open.',
      cloud: 'Sending pages to your cloud GPU and saving the results. Keep the app open.',
      preparing: 'Working out what would be sent',
      starting: 'Loading the model',
      page: 'Page {page} of {total}',
      stopping: 'Stopping. The page in progress is not saved.',
      progressLabel: 'Denoise progress',
      // Closing the dialog no longer ends anything (`state/jobs.svelte.js`).
      background: 'You can close this and keep working. The run goes on and shows under Jobs.',
    },
    consent: {
      title: 'Denoise on your cloud GPU?',
      heading: {
        one: '1 page goes to your cloud GPU',
        other: '{count} pages go to your cloud GPU',
      },
      what: {
        one: '1 whole page, {pixels} pixels',
        other: '{count} whole pages, {pixels} pixels',
      },
      models: 'Models',
      tooLarge: 'Left out',
      tooLargeValue: {
        one: '1 page is too large for this preset and is not sent.',
        other: '{count} pages are too large for this preset and are not sent.',
      },
      costBasis: 'Estimated from the pages\' sizes. Your provider bills actual use.',
      result: 'Denoised pages are saved to {path}. The chapter\'s scans are not changed.',
      project: 'Confirming lets this project send whole pages to this endpoint for denoise without asking again. The provider bills your account for each.',
      standing: 'This project already allows sending pages to this endpoint.',
    },
    summary: {
      written: {
        zero: 'No pages were saved',
        one: '1 page saved',
        other: '{count} pages saved',
      },
      failed: {
        one: '1 page failed',
        other: '{count} pages failed',
      },
      page: 'Page {page}',
      stopped: 'You stopped the run. Pages after the last saved page were not denoised.',
      copied: 'Copied',
    },
    reason: {
      tooLarge: 'Too large for this preset',
      unsupported: 'Not an image this preset can read',
      inference: 'The model could not process it',
      unreadable: 'The page could not be read',
      writeFailed: 'The result could not be saved',
      unavailable: 'The denoise GPU is not available',
      changed: 'The page changed while denoise ran',
      unknown: 'Failed ({code})',
    },
    notice: {
      finished: {
        one: 'Chapter {number} denoised: 1 page saved, {failed} failed.',
        other: 'Chapter {number} denoised: {count} pages saved, {failed} failed.',
      },
      stopped: 'Denoise of chapter {number} did not finish. Run it again from the chapter menu.',
      replaced: {
        one: 'Chapter {number}: 1 page is now its denoised version.',
        other: 'Chapter {number}: {count} pages are now their denoised versions.',
      },
      replacedKept: {
        one: 'Chapter {number}: 1 page is now its denoised version. The other pages stayed as they were ({kept}).',
        other: 'Chapter {number}: {count} pages are now their denoised versions. The other pages stayed as they were ({kept}).',
      },
      replaceFailed: 'Nothing was replaced in chapter {number}: the denoised file for page {page} could not be used. Run Denoise again.',
    },
    error: {
      cloudDisabled: 'Cloud use is off. Turn it on in Settings > Cloud, then try again.',
      noProfile: 'No cloud GPU is selected. Choose one in Settings > Cloud.',
      notSetUp: 'This cloud GPU is not set up for page denoise. In Settings > Cloud, set up with Modal and turn on Page denoise, then try again.',
      openCloud: 'Open Cloud settings',
      modelMissing: 'The local model is missing or failed its check. Go back to download it again.',
      runtimeMissing: 'The ONNX runtime this model runs on is not installed. Download it in Settings > Models.',
      outDir: 'That folder cannot be used. Choose a full folder path you can write to.',
      unknown: 'Denoise did not finish ({code}). Try again.',
    },
  },

  /* ================================================================== */
  /* jobs - runs and denoises going on in the background                 */
  /* ================================================================== */
  // The Jobs button on Home and in the editor, its list, and the question a
  // held quit asks (`state/jobs.svelte.js`, `shell/JobsIndicator.svelte`).
  jobs: {
    title: 'Jobs',
    indicator: {
      running: {
        one: '1 running',
        other: '{count} running',
      },
      finished: {
        one: '1 finished',
        other: '{count} finished',
      },
      // The button's name, and what is announced when the count changes.
      label: {
        zero: 'Jobs: none running',
        one: 'Jobs: 1 running',
        other: 'Jobs: {count} running',
      },
    },
    item: {
      title: '{project} · {chapter}',
      chapter: 'Chapter {number}',
      unknown: 'A chapter',
      progressLabel: '{kindKey} progress',
    },
    kind: {
      detect: 'Detect',
      clean: 'Clean',
      cloudClean: 'Clean on cloud GPU',
      denoise: 'Denoise',
      cloudDenoise: 'Denoise on cloud GPU',
    },
    // A chapter row's own line while a job runs on it.
    active: {
      detect: 'Detecting',
      clean: 'Cleaning',
      cloudClean: 'Cleaning on cloud GPU',
      denoise: 'Denoising',
      cloudDenoise: 'Denoising on cloud GPU',
    },
    row: {
      progress: '{kindKey}, page {page} of {total}',
      starting: '{kindKey}',
    },
    status: {
      starting: 'Starting',
      page: 'Page {page} of {total}',
      stopping: 'Stopping',
      completed: 'Done',
      cancelled: 'Stopped',
      failed: 'Did not finish',
    },
    action: {
      stop: 'Stop',
      open: 'Open',
      openLabel: 'Open {chapter}',
      dismiss: 'Dismiss',
      clear: 'Clear finished',
    },
    quit: {
      title: 'Quit while jobs are running?',
      body: {
        one: '1 job is still running. Quitting stops it. Pages already finished are kept, and a stopped clean can be resumed later.',
        other: '{count} jobs are still running. Quitting stops them. Pages already finished are kept, and a stopped clean can be resumed later.',
      },
      hideNote: 'Keep running hides the window. The jobs go on. Open the app again from the tray icon.',
      stop: 'Stop jobs and quit',
      keep: 'Keep running in background',
    },
  },

  /* ================================================================== */
  /* ladder - the engine rungs                                           */
  /* ================================================================== */
  ladder: {
    rung: {
      fill: 'Fill',
      lama: 'LaMa Manga',
      // The optional rung 3a. Never bundled, never
      // on the default path - but a patch it produced still names it, so a
      // project made on a machine with the sidecar reads correctly on one
      // without it.
      flux: 'FLUX',
      cloud: 'Cloud',
      paint: 'Paint',
      clone: 'Clone',
      unknown: 'Unknown engine',
    },
  },

  /* ================================================================== */
  /* masks - the Layers & review window                                  */
  /* ================================================================== */
  masks: {
    // A layer whose input an earlier edit changed. Rebuild is Try again under
    // another name and carries `action.retryHint`. Undo is the editor's global
    // undo, so its label says "last action" and its tooltip names the action
    // (`editor.action.undoCommand`); it is not scoped to the edit that caused
    // the review.
    dependency: {
      keep: 'Keep result',
      keepHint: 'Keep this layer as it is and mark it reviewed',
      rebuild: 'Rebuild from lower layers',
      undoLast: 'Undo last action',
    },
    filter: {
      needsReview: 'Needs review',
      hint: 'Show only the regions that need review (R)',
    },
    scope: {
      viewport: 'Positions {from} to {to}',
      viewportOne: 'Position {position}',
    },
    empty: {
      noMasks: 'No masks on this page yet.',
      noneNeedReview: 'Nothing here needs review.',
      noText: 'No text was detected in this chapter.',
    },
    title: {
      declined: 'Left as it was',
      gateSkipped: 'Held back by the script gate',
      noMask: 'No mask',
      // A region Detect found and stored, waiting to be cleaned.
      detected: 'Detected text',
    },
    status: {
      // The four states `maskRow()` classifies a region into. `applied` names
      // what it is again, now that `unexamined` carries the case it used to be
      // stretched over: a region with no mask that nothing has declined, held
      // back or flagged. `canvas.region.name` renders these as
      // `{titleKey}, {statusKey}`, so each has to be true beside its title.
      applied: 'Applied',
      needsReview: 'Needs review',
      declined: 'Declined',
      unexamined: 'Not cleaned yet',
      detected: 'Detected',
    },
    sub: {
      noMask: 'nothing applied',
      detected: 'waiting to be cleaned',
      startsWith: 'starts with {engineKey}',
    },
    fillMode: {
      matchSurround: 'Match surround',
      reconstruct: 'Reconstruct',
      solid: 'Solid',
    },
    provenance: {
      engine: 'Engine',
      model: 'Model',
      fillMode: 'Fill',
      elapsed: 'Elapsed',
      cloudCost: 'Cost',
      cloudRequestId: 'Request',
      flagged: 'Flagged',
      applied: 'Applied',
      detected: 'Detected',
      foundOn: 'Found on',
      startsWith: 'Starts with',
    },
    value: {
      elapsed: '{ms} ms',
      // The only currency in the app. `currency` runs Intl.NumberFormat; no
      // other string may write a `$`.
      cloudCost: '{cost:currency}',
      cloudCostUnknown: 'not reported by the provider',
      nothingApplied: 'nothing (the original text is untouched)',
      detectedYes: 'yes, by the automatic pass',
      detectedNo: 'no, the automatic pass missed it',
      nothingYet: 'nothing yet (the original text is untouched)',
      foundLocal: 'this computer',
      foundCloud: 'your cloud GPU',
    },
    // **The models themselves.** These used to be plain verbs - "Quick",
    // "Redraw, stronger" - on the reasoning that a translator should never
    // have to read the ladder's own words. The user ruled the other way:
    // an engine picker exists so that
    // somebody can move between *models*, and a name that hides which model
    // ran makes that impossible. Short names, so a row and a segmented control
    // both have room for them.
    engineChoice: {
      fill: 'Fill',
      lama: 'LaMa Manga',
      // Rung 3a, offered only where the sidecar is
      // actually installed - `rowEngines` in `src/lib/model/masks.js`.
      flux: 'FLUX',
      // Offered by a row's picker and the region menu while a cloud endpoint
      // is ready (`rowEngines` in `src/lib/model/masks.js`), and always the
      // entry of a mask the cloud rendered.
      cloud: 'Cloud',
    },
    action: {
      delete: 'Delete layer',
      deleteHint: 'Delete this layer and bring the original text back',
      deleteRegion: 'Dismiss',
      deleteRegionHint: 'Take this off the list and leave the page as it is',
      retry: 'Try again',
      // Try again and Clean with *replace* this layer's result and read only
      // the layers below it; a new stroke reads the page as shown and refines
      // it (docs/repeated-inpaint-plan.md, "Editing behavior"). `rerunNote`
      // sits under the picker so the difference is visible without a hover.
      retryHint: 'Replace this layer, cleaning again from the layers below it',
      rerunNote: 'Try again replaces this layer. A new stroke refines what you see now.',
      // Paint and clone strokes are copied pixels with nothing to run again
      // (`model/masks.js#reRunnable`). The disabled Try again says so, as its
      // tooltip and as a line in the expanded row.
      retryBlocked: 'Try again is not available for paint and clone strokes. Paint over it or delete it.',
      // The second Try again: the same engine with the area it may redraw
      // 5 px wider than last time, growing again on each press. Plain Try
      // again repeats the last result exactly; this one changes it.
      retryWider: 'Try again wider',
      retryWiderHint: 'Clean again with the same engine over a slightly larger area. Each press grows it a little more.',
      engine: 'Clean with',
      engineHint: 'Replace this layer using another engine, from the layers below it',
      // The region menu's heading over a detected region's two kinds of text,
      // named as Text cleanup's two engine rows name them
      // (`tools.param.bubbleText`, `tools.param.outsideText`): the kind picks
      // which of those rows a later Clean starts the region on.
      textType: 'Text type',
      showOnPage: 'Show on page',
      cleanAnyway: 'Clean anyway',
      // A detected region: clean it here from the mask already found, or on
      // the cloud GPU after the cost is shown and confirmed.
      cleanDetected: 'Clean',
      cleanDetectedCloud: 'Clean on cloud GPU',
      approve: 'Approve and clean',
      opacity: 'Layer opacity',
      locked: 'Lock position',
    },
    // What a layer's native capabilities allow, said under its controls.
    // Moving and turning happen on the page; the row only explains how, or
    // why a redrawn layer does not move (`model/layers.js`).
    layer: {
      moveHint: 'Drag it on the page to move it, or drag its round handle to turn it. Alt with the arrow keys nudges it.',
      lockedNote: 'Locked in place. Unlock it to move or turn it.',
      fixedNote: 'Redrawn from what was under it, so it stays where it was cleaned.',
      resetPosition: 'Reset position',
      resetPositionHint: 'Put this layer back where it was made, upright',
      opacityValue: '{percent} percent',
    },
    // The turn handle on a selected movable layer. It is a slider to a
    // screen reader, so its value reads in degrees.
    transform: {
      rotate: 'Turn layer',
      rotateHint: 'Drag to turn it about its middle. Shift snaps to 15 degree steps. Arrow keys turn it one degree, and 0 stands it upright.',
      degrees: '{degrees} degrees',
    },
    // Why the native side refused a layer change (`LayerRefusal` in
    // `cleaner_core::patch`), rendered inside `notice.layerRefused`.
    refused: {
      noOutput: 'a detection has no cleaned layer to change yet',
      fixed: 'a redrawn layer stays where it was cleaned',
      locked: 'the layer is locked in place',
      noLock: 'a redrawn layer has no position to lock',
    },
    notice: {
      layerRefused: 'That layer change was not made: {reasonKey}. The layer is as it was.',
    },
    menu: {
      // The right-click menu's own name, for a screen reader announcing it.
      // "Layer" rather than "region" or "mask": the panel is called LAYERS and
      // the menu is raised on a row of it, or on the box the row stands for.
      label: 'Layer actions',
      // The selection tool's right-click over bare paper: look again at this
      // spot for text Detect missed, or whose mask was deleted.
      page: 'Page actions',
      detectHere: 'Detect text here',
    },
    hint: {
      showOnPage: 'Scroll to this region and select it',
      cleanAnyway: 'Clean it despite the script gate',
      cleanDetected: 'Clean this region on this computer, from the mask already found',
      cleanDetectedCloud: 'Clean this region on your cloud GPU. The first time in a project, you see the cost and confirm.',
    },
    command: {
      deleteMask: 'a mask deleted',
      deleteRegion: 'a region dismissed',
      rerunMask: 'a mask re-run',
      cleanAnyway: 'a gate-skipped region cleaned',
      layerStyle: 'layer appearance changed',
      layerOpacity: 'layer opacity changed',
      layerLock: 'layer lock changed',
      layerMove: 'layer moved',
      layerRotate: 'layer turned',
    },
  },

  /* ================================================================== */
  /* tools - the tool rail and the tool shell (pill and panel)         */
  /* ================================================================== */
  tools: {
    name: {
      // Internally still `autoClean`: saved sessions, the `1` shortcut and the
      // native run key on the id, and only the words on screen changed.
      autoClean: 'Text cleanup',
      brush: 'Brush',
      shapes: 'Shapes',
      aiMaskBrush: 'AI mask brush',
      contentAwareFill: 'Content-aware fill',
      cloneHeal: 'Clone / heal',
      // Edits the detected area Clean will erase, after a Detect.
      maskSelect: 'Selection',
    },
    // The tooltip on the tool's own name at the left of the bar. It used to be
    // the window header's right-aligned meta; a bar has no room for a second
    // line of text beside the name, and a hint is the one thing here a tooltip
    // is honestly enough for - it is a reminder of the gesture, not a reason
    // anything is unavailable.
    hint: {
      autoClean: 'Enter to run',
      brush: 'drag to paint',
      shapes: 'drag on the page',
      aiMaskBrush: 'stroke over text',
      // `{cloneSourceModifier}` is a context param, not one this call site
      // passes: the tool bar renders `t(spec.hintKey)` over a key the tool
      // table chose, and the modifier is a preference. See `provideContextParam`.
      cloneHeal: '{cloneSourceModifier}-click a source',
      maskSelect: 'add to or remove from the area Clean erases',
    },
    // There is no `note` family. Every tool used to carry a sentence or two
    // above its parameters explaining itself; the bar shows labels and controls
    // now, the one-line `hint` above is all a tool says about itself in the
    // application, and the longer account lives in `docs/features.md`. The
    // in-app Help window that used to hold it was removed with the tool window.
    // The accessible name of a dropdown's trigger, which draws a short label
    // and the value it holds: `In bubbles  manga-LaMa` on the bar reads as
    // "Speech bubble text: manga-LaMa" to a screen reader. The short form is
    // what is *drawn*; this is what is announced.
    label: {
      choice: '{labelKey}: {value}',
      // The Text cleanup panel's progress bar while a run is going. Its value
      // is read out as `editor.run.progress`.
      progress: 'Run progress',
    },
    // The Text cleanup panel's Detect on and Clean on rows. The reasons
    // Cloud GPU cannot be chosen are shared with Settings
    // (`state/cloudtargets.svelte.js`).
    target: {
      // A cloud choice that was made and can no longer run, said before the
      // reason. Kept selectable, so the control never shows a place it is not.
      stranded: 'Text cleanup will not start until the cloud GPU is ready again, or you choose This computer.',
      detectSaveFailed: 'Could not save where detection runs. Try again.',
      // The native refusal of a cloud run holding the Small profile
      // (`cloud_detect_small_unsupported`). The interface never sends one, so
      // this is a caller out of step, and trying again sends the right one.
      cloudSmallRefused: 'Cloud detection uses Ogkalu Full, not Small. Try again, or Detect on This computer.',
      cleanSaveFailed: 'Could not save where cleaning runs. Try again.',
      chooseModels: 'Choose models',
      // Beside `editor.state.detectModelsCloud`: sets Detect on to Cloud GPU.
      useCloud: 'Detect on cloud GPU',
      // Under the mixed-execution checkbox (`cloud.clean.mixed.label`), shown
      // while Clean on is Cloud GPU. Off by default: the cloud cleans it all.
      mixedHint: 'Regions set to Fill or Solid colour are tried here first; any that fail the quality check go to the cloud GPU. Off, every region goes to the cloud GPU.',
    },
    // Where the panel's engine picks send each region (`run.rs`). They are
    // the clean's: drawn for Clean and Detect & clean, and read for every
    // region the run cleans, including regions detected earlier.
    // Under the Text cleanup panel's Mask padding slider.
    padding: {
      hint: 'Grows each detected mask by this many pixels. Detect uses it; Apply changes masks already detected.',
    },
    picks: {
      local: 'Every region is cleaned starting from these picks, including regions detected earlier.',
      // Clean on Cloud GPU: the cloud clean is strictly remote unless the
      // mixed box is ticked (`cloud_clean.rs#CleanExecution`).
      cloud: 'Every region goes to the cloud GPU.',
      mixed: 'Fill and Solid colour are tried on this computer first. The rest go to the cloud GPU.',
    },
    // What a dropdown's trigger calls its parameter, where the full label is
    // too long to sit on a bar in front of its own value: *Speech bubble text*
    // and *Text outside bubbles* are a sentence each, and the bar shows them
    // side by side with a model name after each. The full label is still the
    // control's accessible name, through `tools.label.choice` above.
    short: {
      cleanWith: 'Clean with',
      mode: 'Mode',
    },
    param: {
      // Text cleanup's mode (the run's step), and where each half of it runs.
      step: 'Mode',
      detectOn: 'Detect on',
      cleanOn: 'Clean on',
      scope: 'Scope',
      // Which text a run takes: the languages chosen in Settings, or every
      // text it finds (`session.textPolicy`).
      textPolicy: 'Text',
      bubbleText: 'Speech bubble text',
      bubbleColor: 'Solid fill color',
      maskPadding: 'Mask padding',
      outsideText: 'Text outside bubbles',
      // Whether Text cleanup touches text outside bubbles at all. The row above
      // names the engine; this one is the opt-in the pipeline design always described
      // and never had a control for.
      outsideBubbles: 'Outside bubbles',
      size: 'Size',
      hardness: 'Hardness',
      spacing: 'Spacing',
      mode: 'Mode',
      color: 'Color',
      opacity: 'Opacity',
      flow: 'Flow',
      shape: 'Shape',
      outlineColor: 'Outline color',
      outlineWidth: 'Outline width',
      feather: 'Feather',
      // The AI mask brush's engine row. Deliberately `masks.action.engine`'s
      // words rather than `engine` above: the row on a Layers entry and the
      // chips on the canvas offer the same list of engines under the same
      // names (`src/lib/editor/tools.js#MASK_ENGINES`), and a user who learns
      // one has learned the other.
      cleanWith: 'Clean with',
      alignment: 'Alignment',
    },
    color: {
      hue: 'Hue',
      saturation: 'Saturation',
      brightness: 'Brightness',
      red: 'Red',
      green: 'Green',
      blue: 'Blue',
      hex: 'Hex',
      area: 'Color area',
      areaValue: 'Saturation {saturation}%, brightness {brightness}%',
    },
    // Controls in the tool bar that are not a parameter of the tool.
    //
    // *Adjustments* is the button that opens the popover holding every slider
    // but Size: a bar has room for the
    // one control a hand reaches for constantly and not for six, and the rest
    // are a press away rather than gone.
    //
    // The eyedropper lives in that popover too, and is drawn only where the
    // screen can be sampled: `EyeDropper` in Chromium, AppKit's sampler in the
    // macOS app. It is absent everywhere else (Linux), because the swatch and
    // the hex field are the routes to the same value that every platform has.
    action: {
      adjustments: 'Adjustments',
      // The Text cleanup panel's collapsed group: the two per-run engine
      // picks and the solid fill colour.
      advanced: 'Cleaning models',
      applyPaddingPage: 'Apply to this page',
      applyPaddingChapter: 'Apply to this chapter',
      eyedropper: 'Pick a color from the screen',
    },
    option: {
      stepAuto: 'Detect & clean',
      stepDetect: 'Detect',
      stepClean: 'Clean',
      onLocal: 'This computer',
      onCloud: 'Cloud GPU',
      scopePage: 'Page',
      scopeChapter: 'Chapter',
      // `outsideBubbles`: hold free text for review, or clean it anyway.
      outsideReview: 'Hold for review',
      outsideClean: 'Clean anyway',
      scopeProject: 'Project',
      // `session.textPolicy`: `legacy_gate` and `all_text`.
      policyLegacy: 'Chosen languages',
      policyAll: 'All text',
      // Shapes' `mode` row offers this beside the cleaning rungs, one of
      // which is called *Fill* - rung 0, which measures the paper just outside
      // the mask and paints that one colour on it. This one covers
      // the shape in a colour the user picked. Two very different acts, so the
      // word "fill" is spent on the engine and this one says what it is.
      modeSolid: 'Solid colour',
      rect: 'Rect',
      ellipse: 'Ellipse',
      lasso: 'Lasso',
      polygon: 'Polygon',
      line: 'Line',
      engineCloud: 'Cloud',
      // Beside the Cloud engine while it cannot be chosen, with the one
      // button that fixes it.
      engineCloudNotReady: 'No cloud GPU is ready. Set one up in Settings.',
      engineMissing: 'This model is not installed. Download it in Settings › Models.',
      engineCloudSettings: 'Open Cloud settings',
      // There is no `engineFill` / `engineRedraw` pair any more. Text cleanup's
      // two rows named a *family* - "Fill", "Redraw" - and the user ruled that
      // a picker must name the model; both rows
      // read `masks.engineChoice.*` now, which is the Layers row's own list.
      aligned: 'Aligned',
      nonAligned: 'Non-aligned',
      clone: 'Clone',
      heal: 'Heal',
      // The selection tool's two modes and its round brush, drawn as icons;
      // these are their accessible names and tooltips.
      maskAdd: 'Add to selection',
      maskRemove: 'Remove from selection',
      brush: 'Brush',
    },
  },

  /* ================================================================== */
  /* shortcuts - the sheet                                               */
  /* ================================================================== */
  shortcuts: {
    group: {
      tools: 'Tools',
      view: 'View',
      edit: 'Edit',
      zoom: 'Zoom',
      navigation: 'Navigation',
      panels: 'Panels',
      app: 'App',
      // Not one of `SHORTCUT_GROUPS`: the sheet's last section is the binding
      // that is a modifier plus a click rather than a chord, and the shortcut
      // table has no row for it because the table is keys.
      pointer: 'Pointer',
    },
    sheet: {
    },
    // Rebinding. Every refusal names what it refused and what to do instead;
    // `refusedConflict` quotes the shortcut already holding the combination,
    // because "that key is taken" without saying by what is a dead end.
    rebind: {
      change: 'Change the shortcut for {name}',
      reset: 'Reset {name} to its default',
      resetAll: 'Reset all to defaults',
      listening: 'Press a key…',
      unbound: 'Unbound',
      hint: 'Press the combination to use. Escape cancels, Backspace clears the shortcut.',
      fixedHint: 'Escape always cancels, and cannot be changed.',
      refusedAlt:
        'Alt combinations belong to the operating system and are never intercepted here. Choose another combination.',
      refusedKey: 'That key cannot carry a shortcut. Choose another combination.',
      refusedConflict:
        '{nameKey} already uses that combination. Choose another, or change that shortcut first.',
      refusedFixed: 'Escape always cancels, and cannot be rebound or cleared.',
      refusedUnknown: 'This build has no such shortcut.',
    },
    tool: {
      maskSelectShape: 'Selection: next shape',
      maskSelectMode: 'Selection: swap add and remove',
    },
    view: {
      holdOriginal: 'Hold to show the original',
      pinOriginal: 'Pin the original',
      maskOverlay: 'Mask overlay',
      reviewFilter: 'Needs-review filter',
    },
    edit: {
      undo: 'Undo',
      redo: 'Redo',
      deleteLayer: 'Delete the selected layer',
    },
    zoom: {
      fit: 'Fit the page',
      in: 'Zoom in',
      out: 'Zoom out',
      actualSize: 'Actual size, 1:1',
    },
    page: {
      left: 'Page left',
      right: 'Page right',
    },
    review: {
      next: 'Next region needing review',
      prev: 'Previous region needing review',
    },
    panel: {
      pages: 'Pages',
      masks: 'Layers & review',
      tools: 'Tool options',
    },
    app: {
      export: 'Export',
      newProject: 'New project',
      openProject: 'Open project',
      settings: 'Settings',
      shortcutSheet: 'This list',
      home: 'Library',
      cancel: 'Cancel or close',
    },
  },

  /* ================================================================== */
  /* time - relative timestamps                                          */
  /* ================================================================== */
  time: {
    relative: {
      justNow: 'just now',
      hoursAgo: {
        one: '1 hour ago',
        other: '{count} hours ago',
      },
      yesterday: 'yesterday',
      daysAgo: {
        one: '1 day ago',
        other: '{count} days ago',
      },
      lastWeek: 'last week',
      weeksAgo: {
        one: '1 week ago',
        other: '{count} weeks ago',
      },
    },
  },

  /* ================================================================== */
  /* notice - the bottom-left queue. Every one of these is a report on    */
  /* something that already happened.                                    */
  /* ================================================================== */
  notice: {
    // The cloud clean batch's own notices (`inference/cloud_clean.rs`). Each
    // with a cause has a form without it, for a code the catalogue has no
    // words for (`model/cloudnotices.js`).
    cloudClean: {
      regionFailed: 'A region on page {page} was not cleaned on the cloud GPU. It stays detected.',
      regionFailedBecause: 'A region on page {page} was not cleaned: {codeKey}. It stays detected.',
      regionSkipped: 'A region on page {page} was not sent. It is left as it is.',
      regionSkippedChanged: 'A region on page {page} changed after you confirmed, so it was not sent.',
      regionSkippedGone: 'A region on page {page} was deleted after you confirmed, so it was not sent.',
      regionSkippedUnresolved: 'A region on page {page} was not sent: an earlier cloud request for it has not been resolved.',
      stopped: 'The cloud clean stopped at page {page}. The regions not yet cleaned stay detected.',
      stoppedBecause: 'The cloud clean stopped at page {page}: {codeKey}. The regions not yet cleaned stay detected.',
      code: {
        cloudDisabled: 'cloud engines were turned off',
        gatewayUnauthorized: 'your cloud GPU refused the app’s key',
        credentialMissing: 'the cloud key is missing',
        profileChanged: 'your cloud setup changed',
        gatewayUnreachable: 'your cloud GPU could not be reached',
        gatewayError: 'your cloud GPU answered with an error',
        gatewayProtocol: 'your cloud GPU sent an answer the app could not read',
        consentInvalid: 'its consent was no longer valid',
        remoteFailed: 'the cloud GPU could not clean it',
        recipeChanged: 'the cloud model changed',
        gpuChanged: 'the cloud GPU or its price changed, or could not be checked',
        chunkRefused: 'a batch did not match what you confirmed',
        pollTimeout: 'the cloud GPU took too long to answer',
        submissionUnknown: 'the app could not tell whether it was sent',
      },
    },
    paint: { colorApproximated: 'This page uses a palette or limited gray levels. The selected color was replaced by the closest available color.' },
    run: {
      busy: 'Another run is starting or already running. Wait for it to finish.',
      // The backend runs a few chapters at once (`run.rs`, MAX_RUNS).
      atCapacity: 'The most runs that can go at once are already going. Wait for one to finish, then start this one.',
      finished: {
        select: 'pages',
        zero: 'Text cleanup finished: no pages needed cleaning.',
        one: 'Text cleanup finished: 1 page cleaned, {regions} regions.',
        other: 'Text cleanup finished: {pages} pages cleaned, {regions} regions.',
      },
      cancelled: 'Text cleanup cancelled. The pages already cleaned are kept.',
      nothingInScope: 'Nothing to clean in this scope.',
      // Clean on its own cleans only what Detect stored.
      nothingDetected: 'Nothing detected to clean here. Run Detect first.',
      // A Detect of one spot that stored nothing: no text there, or the page
      // holds its mask already.
      nothingNewHere: 'No new text found here.',
      // Detect stores what it finds; nothing is cleaned until Clean runs. A
      // detect that found nothing says `notice.chapter.emptyResult` instead.
      detected: {
        select: 'regions',
        one: 'Detect finished: 1 region found. Nothing is cleaned yet.',
        other: 'Detect finished: {regions} regions found. Nothing is cleaned yet.',
      },
      allLanguagesSkipped: 'All source languages are skipped. Select a language to clean.',
      ocrRescueUnavailable: 'The text reader is off for this run: {reason}',
      accelFallback: '{model} switched from {requested} to {effective}: {reason}',
      // The run could not start because the weights are not on this machine.
      // A notice rather than an error now that there is somewhere to send the
      // reader: before Settings › Models existed, this was a rejected promise
      // the interface had nowhere to put.
      modelsMissing: 'The models are not installed. Download them in Settings › Models.',
      // Pages the run **could not** clean, counted separately from the pages it
      // cleaned, and said even when the number is every page in the chapter.
      // That last case is why this exists: a run where every page failed
      // cleaned no regions, and a zero there used to close the run on
      // `notice.chapter.emptyResult` - "No text found across 0 pages" - which
      // is a dead engine filing a report about the artwork. The pages are left
      // queued rather than marked cleaned, so the sentence says so: nothing has
      // to be undone before running again.
      pagesFailed: {
        select: 'pages',
        one: '1 page could not be cleaned. It is still queued.',
        other: '{pages} pages could not be cleaned. They are still queued.',
      },
      // A model that built, ran, and then stopped answering. Both parameters
      // are keys, resolved before they are interpolated: `models.kind.*` names
      // the model in the words the loaded-models tab uses, and `accel.*` names
      // the provider. Neither is assembled here.
      //
      // The three sentences are three different things the reader needs, in the
      // order they need them: what happened, that it is not permanent, and what
      // to change if it is. On Windows the usual cause is the graphics driver's
      // watchdog resetting the card mid-inference, which poisons the session -
      // so every later run in the same session fails too unless the model is
      // dropped, and that is what "unloaded" is reporting.
      engineFault:
        'The {modelKey} stopped answering on {accelKey}, so it was unloaded. The next run builds it again. If it keeps happening, choose CPU in Settings › Performance.',
    },

    // Downloading or replacing the engine runtime itself. Both of these are
    // refusals rather than failures - nothing was half-written - and both name
    // the one thing that would make the press work.
    runtime: {
      // Windows will not let a file that is mapped into a running process be
      // replaced, and the runtime is mapped as soon as anything asks what this
      // machine can accelerate. So the remedy is not "try again": it is a
      // restart, and then the download before anything loads it again.
      inUse:
        'The engine runtime is loaded, and a file in use cannot be replaced. Quit Manga Cleaner, open it again, and download the runtime before you clean anything.',
      // Both figures are byte counts and both are formatted here rather than by
      // whoever counted them, for `models.value.size`'s reason: a size is read,
      // not translated. Saying both is the point - "not enough space" alone
      // leaves the reader guessing how much to clear.
      noSpace:
        'There is not enough free disk space for that download: {needed:memory} needed, {free:memory} free. Free some space and press Download again.',
    },

    // A model download that failed while no screen showing its row was open
    // (`api/model-download-notices.js`). `{nameKey}` is the name Settings gives
    // that row: a file group's, the runtime's, or a file's `models.kind.*`,
    // never the raw id. `{error}` is the backend's own reason, verbatim.
    download: {
      failed: '{nameKey} could not be downloaded: {error}. Try again from Settings.',
      // The id matched no row this build knows, or the catalogue could not be
      // read to name it.
      failedUnnamed: 'A model could not be downloaded: {error}. Try again from Settings.',
    },
    chapter: {
      emptyResult: 'No text found across {pages} pages. Nothing to clean.',
      added: 'Ch. {chapter} added to {project}.',
      numberTaken: 'Ch. {chapter} already exists in {project}.',
      deleted: 'Ch. {chapter} deleted. The scans were left where they are.',
      deletedWithScans: 'Ch. {chapter} deleted, and its scans with it.',
      // Asked for and refused: the folder is the project's own, and every other
      // chapter of the project reads it by default.
      deletedProjectFolderKept: 'Ch. {chapter} deleted. Its scans were kept: they are the project’s own folder.',
      deletedSourceUnknown: 'Ch. {chapter} deleted. The library had no record of where its scans are, so none were removed.',
      // Refused rather than created. Two chapters over one folder would hold
      // the same pages under two numbers, and there is no way to correct a
      // chapter's folder afterwards - so the chapter is not made, and the name
      // of the one that already has the folder is what tells the user which
      // folder is meant.
      sourceTaken:
        '“{chapter}” already reads that folder. Choose a folder for this chapter. Two chapters over one folder would hold the same pages twice.',
    },
    convert: {
      finished: 'Converted {count} files to {format}. The originals are kept.',
    },
    project: {
      created: 'Project created. {modeKey} mode is now fixed for every chapter.',
      renamed: 'Renamed to {name}.',
      deleted: '{name} removed from the library. The pages on disk are untouched.',
      sourcePathCopied: 'Source path copied: {path}',
      sourcePathCopyFailed: 'The source path could not be copied to the clipboard.',
      // Not the same as dismissing the chooser, which is a choice and passes in
      // silence. This is the chooser refusing to open, and it says what still
      // works: the field beside the button takes a typed path.
      sourcePickFailed: 'The folder chooser could not be opened. Type the path instead.',
    },
    library: {
      changeFailed: 'That change could not be saved. The library is as it was.',
      ingestInsufficientSpace: 'Not enough free space to import this chapter.',
      ingestSpaceUnknown: 'Could not check free space for this chapter import.',
    },
    history: {
      saveFailed: 'Could not save undo history. Undo may not work after a restart.',
    },
    job: {
      resumed: 'Resuming at page {page}.',
      busy: 'Another Manga Cleaner process is using this chapter. Nothing was changed; try again when it has finished.',
      stale: 'This chapter was changed outside this window, so that change was not saved. The chapter has been read again.',
    },
    // The pressure ladder, one notice per step. Each step is
    // recoverable - the regions route down the ladder and the run continues -
    // so these say what the run will do differently rather than that something
    // went wrong.
    memory: {
      // The first step, and it only happens on a run that had rung 3a running -
      // which an automatic run never does. Ahead of
      // the inpainter because it is both the largest resident thing and the
      // only optional one.
      refusedSidecar:
        'This machine is short of memory, so the FLUX sidecar was stopped and will not be started again this run. Regions that needed it fall back to the default inpainter.',
      unloadedInpainter:
        'This machine is short of memory, so the inpainter was unloaded. Regions that needed it are left as they were and listed for review.',
      oneWindow: 'Still short of memory. Down to one page region at a time.',
    },
    input: {
      cmykPreview: '{file}: CMYK previews use its embedded profile when present. Untagged CMYK uses a multiplicative-ink display assumption; original native ink values remain unchanged.',
      archivedOriginal: '{file}: original bytes are archived in {path} (SHA-256 {sha256}).',
      firstFrame: '{file}: using the first visible frame of {count}; the complete animation is archived.',
      missingArchive: '{file}: this older conversion has no archived original. The working page and edits remain available.',
      fileSkipped: {
        one: '1 file skipped: {file} ({reasonKey}).',
        other: '{count} files skipped, starting with {file} ({reasonKey}).',
      },
      junkSkipped: {
        one: '1 junk entry skipped.',
        other: '{count} junk entries skipped.',
      },
      duplicateBasename: 'Multiple files named {file} have different extensions. All distinct images were kept.',
      // Not a refusal: these pages are in the chapter. The sentence says what
      // was done and to how many, because the editor works on the PNGs from
      // here on and the originals are still where they were.
      converted: {
        one: '1 {from} page was converted to PNG. Your original is untouched.',
        other: '{count} {from} pages were converted to PNG. Your originals are untouched.',
      },
      joinAnomaly: 'Duplicated overlap rows between positions {first} and {second}.',
    },
    cloud: {
      // The whole user-facing content of a privacy guarantee: the request was
      // refused in the interface, before any adapter call, so the page did not
      // leave the machine. It has to say that, not merely that cloud is off.
      blocked: 'Cloud GPU is off in Settings. Nothing was sent. No part of this page left your machine.',
      notReady: 'No cloud GPU is ready. Nothing was sent. Set one up in Settings, Cloud.',
      consentFailed: 'The cloud request could not be prepared. Nothing was sent.',
      permissionFailed: 'The cloud setting could not be saved. It is unchanged.',
      // How a render ended: exactly one of these per render.
      finished: 'Cloud render finished in {seconds} s.',
      cancelled: 'Cloud render cancelled.',
      unknown: 'It is not known whether the cloud render ran. Nothing was applied, and it was not sent again.',
      failed: 'The cloud render did not finish. {reasonKey}',
      cancelFailed: 'The cloud render could not be cancelled. It may still finish.',
      recovered: {
        one: 'A cloud render from last time finished and was applied.',
        other: '{count} cloud renders from last time finished and were applied.',
      },
      needsAttention: {
        one: 'A cloud render from last time needs attention. See Settings, Cloud.',
        other: '{count} cloud renders from last time need attention. See Settings, Cloud.',
      },
      setupDone: 'Your cloud GPU is set up and selected.',
      setupFailed: 'Cloud setup did not finish. Open Settings, Cloud, to resume or clean up.',
      cleanupDone: 'Cloud resources deleted.',
      cleanupFailed: 'Some cloud resources could not be deleted. Open Settings, Cloud, to try again.',
      endpointRemoved: 'Removed {name} from this computer.',
      // The phase line of a render in flight (IC-3).
      phase: {
        preparing: 'Preparing',
        submitting: 'Sending',
        queued: 'Waiting for a GPU',
        running: 'Rendering',
        downloading: 'Receiving the result',
        compositing: 'Applying the result',
      },
      // Why a render did not finish, one sentence per group of error codes.
      error: {
        disabled: 'Cloud GPU is off in Settings.',
        target: 'The cloud endpoint changed or is missing. Check Settings, Cloud.',
        credential: 'The endpoint has no access token on this computer. Add one in Settings, Cloud.',
        consent: 'The approval for this render was no longer valid. Try again.',
        region: 'The region changed while it was in the cloud, so the result was not applied.',
        regionUnsupported: 'This region cannot be rendered in the cloud.',
        busy: 'This region is already rendering in the cloud.',
        local: 'Something went wrong on this computer. Nothing was changed.',
        result: 'The endpoint sent back a result that could not be used. Nothing was changed.',
        unauthorized: 'The endpoint refused the access token. Replace it in Settings, Cloud.',
        gateway: 'The endpoint answered with an error.',
        unreachable: 'The endpoint could not be reached.',
        submission: 'It is not known whether the endpoint received the render. It was not sent again.',
        remote: 'The render failed on the cloud GPU.',
        weights: 'This cloud setup is missing required model files. Repair it in Settings, Cloud.',
        remoteCancelled: 'The provider cancelled the render.',
        cancelled: 'It was cancelled.',
        timeout: 'The render took too long and was stopped.',
        generic: 'Something went wrong.',
      },
      // The status element, bottom left, while renders are in flight.
      job: {
        title: 'Cloud',
        elapsed: 'Time since it started',
        page: 'Page {page}',
        region: 'One region',
        cancel: 'Cancel',
        cancelling: 'Cancelling…',
        firstRun: 'The first render can take 1 to 3 minutes while the GPU starts.',
      },
    },
    mask: {
      deleted: 'Mask deleted. The original text under it is back.',
      rerunStronger: 'Region re-run one rung stronger: {rungKey}.',
      rerunSimpler: 'Region re-run one rung simpler: {rungKey}.',
      // The two the Layers row's own controls send. `{rungKey}` is the
      // engine's real name here, not the picker's plain-language one: a notice
      // is a record of what ran.
      rerunAgain: 'Region cleaned again: {rungKey}.',
      rerunWider: 'Region cleaned again over a larger area: {rungKey}.',
      rerunEngine: 'Region cleaned again with {rungKey}.',
      fillMode: 'Fill mode is now {fillModeKey}.',
      reopened: 'Mask kept, reopened in {toolKey}.',
      // What a region edit says when no engine would produce a patch for it.
      // The region is left exactly as it was - the same rule
      // holds for a hand edit as much as for a run - so the sentence has
      // to say that nothing changed, or a user reads silence as success.
      rerunFailed: 'That area could not be cleaned: {reasonKey}. It is exactly as it was.',
      // The selection tool's add or remove was refused. Nothing changed.
      selectionFailed: 'Could not change the selection. The page is as it was.',
      paddingApplied: 'Mask padding is now {px} px. Detected regions changed: {count}.',
      paddingMerged: 'Masks the padding joined are now one mask. Masks merged: {count}.',
      paddingNothing: 'Nothing to change. No detected masks here, or they already have this padding.',
      paddingFailed: 'Could not change the mask padding. The masks are as they were.',
      paddingRefreshFailed: 'Mask padding was saved, but the page could not be refreshed. Reopen the chapter to see the updated mask.',
      // The region menu's Text type was refused: the region was cleaned or
      // removed meanwhile, or could not be saved. Nothing changed.
      typeFailed: 'Could not change the text type. The region is as it was.',
    },
    tool: {
      cloneSourceSet: 'Clone source sampled. Paint to copy from it.',
      cloneNeedsSource: 'Alt-click a source first. Clone and heal copy from somewhere.',
    },
    gate: {
      cleanedAnyway: 'Cleaned despite the script gate.',
    },
    export: {
      invalidSource: 'Export cannot preserve this page as requested. Choose per-page lossless output or correct the source metadata.',
      stalePlan: 'The chapter changed after the export preview. Review the updated output plan.',
      partial: 'Only some output files were published. Review the completed files and retry.',
      finished: 'Exported {count} pages as {format} to {path}.',
      // The stitched sentence carries the gutter count, because a file that
      // invented pixels should say how many on the way past.
      stitched: {
        zero: 'Exported the chapter as one {format} file to {path}. No pixel was invented: every page is the full width.',
        one: 'Exported the chapter as one {format} file to {path}. 1 pixel of paper white was written beside a narrower page.',
        other:
          'Exported the chapter as one {format} file to {path}. {count} pixels of paper white were written beside the narrower pages.',
      },
      refusedOverwrite: 'Export refused: it would replace the files in {path}. Nothing was written.',
      // The seven refusals that used to be silence or a substituted format.
      // Each says what was asked for, what happened instead - nothing - and
      // what would work, because a refusal with no way out of it is a dead end
      // dressed as an explanation.
      refusedLossyFormat:
        '{format} is a lossy format, and this build has no way to tell you what it would cost your pages before it wrote them. Nothing was written. Export PNG or TIFF, which keep every pixel.',
      refusedLayeredFormat:
        '{format} is not written by this build. Nothing was written. Export PSD, which carries the same layers for pages up to 30 000 pixels a side.',
      refusedUnknownFormat:
        'This build cannot write {format}. Nothing was written. The formats it can write are PNG, TIFF, PSD and CBZ.',
      refusedMaskLayers:
        'A CBZ is read page by page, so a mask file inside it would show as a page. Nothing was written. Export PNG or TIFF for a mask file beside each page, or PSD for a layer mask on each region.',
      refusedStitchedLayered:
        'A PSD holds one whole page as its Background, and this build never holds the whole chapter as one image. Nothing was written. Export PSD per page, or one PNG or TIFF for the chapter.',
      refusedLayeredMode:
        'PSD cannot carry an indexed-colour page or one below 8 bits, and converting would change pixels you did not ask to change. Nothing was written. Export PNG, which carries them as they are.',
      refusedLayeredSize:
        'A page here is over 30 000 pixels on a side, which is more than a PSD can hold. Nothing was written. Export PNG or TIFF, which have no such limit.',
      refusedLayeredFileSize:
        'This PSD would exceed its 2 GB file size limit. Nothing was written. Export PNG or TIFF instead.',
      refusedLayeredCount:
        'This page has more layers than a PSD can hold. Nothing was written. Export a flattened PNG or TIFF instead.',
      refusedDestination:
        'That destination is not somewhere this export can write. Nothing was written. Choose a new folder, or give a full path from the top of the disk.',
      refusedStitchPaginated:
        'One file for the chapter is a longstrip layout, and this project is paginated. Nothing was written. Its pages are separate images, and stacking them would make a document that has no original.',
      refusedStitchedArchive:
        'A CBZ holds one file per page, so it cannot hold one file for the chapter. Nothing was written. Export the chapter as a single PNG or TIFF, or keep the archive and export per page.',
      // One per `StitchRefusal`, and each names the condition rather than the
      // page: `reasonKey` is the whole of what crosses the seam, so a sentence
      // naming a page number would be naming one nothing carried.
      stitchRefused: {
        noPages: 'There are no pages to stitch into one file. Nothing was written.',
        mixedMode:
          'These pages are not all in the same colour mode, and one file has one colour mode. Nothing was written. Converting them would change pixels you did not ask to change. Export per page instead.',
        mixedDepth:
          'These pages are not all the same bit depth, and one file has one bit depth. Nothing was written. Export per page instead.',
        mixedProfile:
          'These pages do not all carry the same colour profile, and one file has one profile. Nothing was written. Export per page instead.',
        indexed:
          'These pages use indexed colour, and one file would need one palette across all of them. Nothing was written. Reconciling the palettes would shift colours, so export per page instead.',
      },
    },
  },

  /* ================================================================== */
  /* update - in-app auto-updater                                       */
  /* ================================================================== */
  update: {
    title: 'Update available',
    field: {
      version: 'Version',
      releaseNotes: 'Release notes',
    },
    notes: {
      empty: 'No release notes.',
    },
    status: {
      downloading: 'Downloading update…',
      upToDate: 'Manga Cleaner is up to date.',
      checkFailed: 'Could not check for updates.',
    },
    action: {
      check: 'Check for updates',
      checking: 'Checking for updates…',
      download: 'Download & install',
      downloading: 'Downloading…',
      downloadingPercent: 'Downloading {percent}%…',
      restart: 'Restart Manga Cleaner',
      later: 'Later',
      available: 'Update available',
      availableTag: 'Update available: {version}',
      updateTo: 'Update to v{version}',
    },
  },

  /* ================================================================== */
  /* workflow - the text-shaped review, and the same analysis without a  */
  /* chapter in Settings. Refusals are named outcomes, never the         */
  /* backend's own sentence: see `src/lib/dialogs/workflowoutcome.js`.  */
  /* ================================================================== */
  workflow: {
    title: {
      review: 'Text-shaped review',
      independent: 'Independent model analysis',
    },
    intro: {
      review: 'Finds lettering pixels on one page for you to review. Nothing is erased until you approve a component. No OCR and no language check. Nothing leaves this computer unless you choose cloud analysis and confirm what is sent.',
      independent: 'Reviews model evidence in original page coordinates. Analysis erases nothing and starts no cloud job. The legacy cleaner above is unchanged.',
    },
    rule: {
      writesW: 'Apply writes only the orange W pixels. Nothing else on the page changes.',
      refine: 'A new correction refines this component’s current plan. Try again in Layers replaces the patch, rebuilt from the layers below it.',
      reviewOnly: 'CPU and JPEG results are review-only. Writing needs a PNG page and the qualified GPU setup.',
    },
    field: {
      page: 'Chapter page',
      pageOption: 'Page {number}',
      workflow: 'Workflow',
      rtProfile: 'Ogkalu comic text & bubble detector: profile and page layout',
      rtBackend: 'Ogkalu comic text & bubble detector backend',
      samBackend: 'SAM-TS-L lettering mask backend',
      source: 'Source page',
      sourcePlaceholder: 'Absolute image path',
      padding: 'Padding (source px)',
      brush: 'Brush radius (source px)',
      correction: 'Correction',
      zoom: 'Zoom',
    },
    aria: {
      padding: 'Padding in source pixels',
      brush: 'Correction brush radius in source pixels',
      correction: 'Mask correction mode',
      zoom: 'Preview zoom',
      canvas: 'Source page mask correction canvas. Arrow keys move the cursor. Enter selects the component under it, or paints in Add or Remove mode.',
      preview: 'Source pixel mask preview',
      image: 'Source page under review',
      candidates: 'Review candidates',
      legend: 'Preview legend',
    },
    preset: {
      ctd: 'CTD only: Comic Text Detector (CTD) boxes',
      ctdRegions: 'CTD with Ogkalu comic text & bubble detector regions',
      ctdMask: 'CTD with SAM-TS-L lettering mask pixels',
      ctdTextShape: 'CTD, Ogkalu comic text & bubble detector and SAM-TS-L lettering mask together',
      regions: 'Regions only: Ogkalu comic text & bubble detector text and bubble boxes',
      mask: 'Mask only: SAM-TS-L lettering mask pixels, no region boxes',
      textShape: 'Text-shaped review: Ogkalu comic text & bubble detector context with the SAM-TS-L lettering mask',
    },
    rtProfile: {
      full: 'Full, two vertical tiles',
      small: 'Small, whole page',
    },
    backend: {
      cpu: 'CPU',
      webgpu: 'WebGPU',
      autoSettings: 'Automatic · uses Performance settings',
      canWrite: '{name} · can write',
      reviewOnly: '{name} · review only',
      unavailable: '{name} · unavailable',
      qualified: '{name} · qualified',
      unqualified: '{name} · unqualified',
      // The review's cloud choice: analysis on the user's own endpoint, after
      // consent. Chosen for one stage, it is chosen for every stage the
      // workflow needs, because one review sends to one place.
      cloud: '☁ Cloud GPU · review only',
      cloudOff: '☁ Cloud GPU · not set up',
      cloudCtd: '☁ Cloud GPU · not with CTD',
    },
    action: {
      analyze: 'Analyze for review',
      cancel: 'Cancel analysis',
      prepare: 'Prepare write preview',
      rebuild: 'Rebuild preview',
      reanalyze: 'Analyze again',
      clear: 'Clear corrections',
      apply: 'Apply approved component',
      choosePage: 'Choose page',
      importFullRt: 'Import Full detector graph',
      downloadFullRt: 'Download Full detector graph',
      removeFullRt: 'Remove Full detector graph',
      importSam: 'Import SAM-TS-L graphs',
      installSam: 'Install SAM-TS-L graphs automatically',
      removeSam: 'Remove SAM-TS-L graphs',
      verifySam: 'Verify SAM-TS-L graphs',
      refresh: 'Refresh readiness',
      zoomFit: 'Fit',
      zoomActual: '1:1',
      zoomComponent: 'Zoom to component',
    },
    zoom: {
      fitLabel: 'Fit the page to the view',
      actualLabel: '1:1, one source pixel per screen point',
      value: '{percent}%',
    },
    status: {
      analyzingPage: 'Analyzing page {page} locally…',
      analyzing: 'Analyzing locally…',
      cancelling: 'Cancelling…',
      working: 'Working locally…',
    },
    model: {
      section: 'Models and backends',
      rt: 'Ogkalu comic text & bubble detector',
      sam: 'SAM-TS-L lettering mask',
      write: 'Component write',
      coo: 'COO MTSv3 SFX finder',
      runtime: 'ONNX Runtime',
    },
    cap: {
      rtFull: 'Full tiled graph: {statusKey}',
      rtSmall: 'Small whole-page graph: {statusKey}',
      samState: '{statusKey}. {memoryKey}. Mask only, no OCR.',
      writeQualified: 'Qualified here: WebGPU analysis of a PNG page can prepare approved writes.',
      writeReviewOnly: 'Review only on this computer.',
      cooAbsent: 'Not included: its model rights are unresolved.',
      runtimeInstalled: 'Installed',
      runtimeMissing: 'Install it in Performance before analysis.',
    },
    state: {
      verified: 'SHA-256 verified',
      missing: 'missing',
      installLater: 'install it under Models',
      unverified: 'Verification pending or failed',
      samMissing: 'Graphs missing',
      memoryReady: 'Memory available',
      memoryShort: 'Needs 10 GB of process memory',
    },
    detail: {
      rtIdentity: 'Ogkalu comic text & bubble detector (Full) identity and file',
      rtFile: 'Revision {revision}. {name}, {size} MB, SHA-256 {sha}.',
      samIdentity: 'SAM-TS-L lettering mask identity and files',
      samRevision: 'Revision {revision}. Import both graphs together from an export you obtained yourself.',
      samFile: '{name}, {size} MB, SHA-256 {sha}',
      backends: 'Backend availability and validation',
      backendRow: '{family} {name}: {platform}, {statusKey}. {note}',
      qualified: 'qualified',
      unqualified: 'unqualified',
      technical: 'Technical detail',
      provenance: 'Analysis details',
      sourceSha: 'Source SHA-256',
      maskSha: 'Unchanged SAM-TS-L mask SHA-256',
      models: 'Models',
      modelsValue: 'Ogkalu {rtProfile} on {rtBackend}, SAM-TS-L on {samBackend}',
      timings: 'Timings',
      timingsValue: 'Cold load: Ogkalu {rtLoad} ms, SAM-TS-L {samLoad} ms. Page: SAM-TS-L {samPage} ms, Ogkalu {rtPage} ms.',
      nodes: 'ORT assignment',
      nodesValue: 'WebGPU nodes: encoder {encoder}, head {head}. CPU fallback: encoder {cpuEncoder}, head {cpuHead}.',
      off: 'off',
    },
    result: {
      components: { zero: 'No components', one: '1 component', other: '{count} components' },
      regions: { zero: 'No detector boxes', one: '1 detector box', other: '{count} detector boxes' },
      groups: { zero: 'No text groups', one: '1 text group', other: '{count} text groups' },
      flagged: { zero: 'None flagged', one: '1 flagged with a reason', other: '{count} flagged with a reason' },
      canWrite: 'Analyzed on WebGPU. Components can be prepared for writing.',
    },
    list: {
      components: 'Components awaiting selection',
      regions: 'Detector boxes',
      pixels: '{count} px',
    },
    tag: {
      bubble: 'Bubble',
      text: 'Text box',
      noBox: 'No detector box',
      held: 'Held',
      unassigned: 'No text box claims it',
      isolated: 'Lone mark',
      crossesBalloon: 'Crosses bubble edge',
      written: 'Written',
      contextOnly: 'Context only',
      noMask: 'No SAM-TS-L pixels',
    },
    regionKind: {
      bubbleContext: 'Bubble box',
      textBubble: 'Text in bubble',
      textFree: 'Free text',
      ctdText: 'CTD text box',
    },
    panel: {
      empty: 'Select a component in the list, or click it on the page.',
      region: 'Detector boxes only locate text. They cannot be written.',
      unassigned: 'No text box claims this lettering, so it is a candidate, not a detected problem. Select it only if it is text.',
      isolated: 'A lone mark with no lettering beside it. It may be artwork: select it only if it is text.',
      crossesBalloon: 'This lettering reaches outside its speech bubble. Check the tint against the bubble edge before approving.',
      group: { one: 'Text group {id}: 1 component. Detection cleans the group as one layer.', other: 'Text group {id}: {count} components. Detection cleans the group as one layer.' },
      heldGroup: { one: 'Text group {id}: 1 component. Detection lists the group and leaves it untouched.', other: 'Text group {id}: {count} components. Detection lists the group and leaves it untouched.' },
    },
    permission: {
      label: 'Allow components outside speech bubbles',
      help: 'Off by default. Applies to this review only; the Text cleanup setting stays as it is.',
    },
    correction: {
      inspect: 'Inspect',
      add: 'Add pixels',
      remove: 'Remove pixels',
      pending: 'Pending: {added} px to add, {removed} px to remove. Not in W until the preview is prepared.',
    },
    write: {
      summary: 'W: {count} source px, padding {padding} px',
      identity: 'Plan identity',
      plan: 'Plan',
      support: 'Support SHA-256',
      source: 'Source',
      underlay: 'Underlay',
      renderer: 'Renderer',
      approve: 'I approve writing exactly the orange pixels (W) of {component} on page {page}.',
    },
    legend: {
      evidence: 'SAM-TS-L mask',
      write: 'W, will be written',
      add: 'Pending add',
      remove: 'Pending remove',
      locator: 'Box, locator only',
    },
    outcome: {
      cancelled: 'Analysis cancelled',
      unavailable: 'Needs the desktop app',
      modelMissing: 'Model missing',
      notReady: 'Not ready to analyze',
      modelFailed: 'Model failed',
      memory: 'Not enough memory',
      sourceLimit: 'Page cannot be analyzed',
      undiscovered: 'Nothing found',
      held: 'Held by policy',
      declined: 'Review only',
      needsCorrection: 'Mask needs correction',
      stale: 'Preview out of date',
      expired: 'Analysis out of date',
      badReconstruction: 'Fill could not be rebuilt',
      applied: 'Written to page {page}',
      failed: 'Something went wrong',
      cooAbsent: 'SFX finder not included.',
      invalidRequest: 'Request not accepted',
      wrongModel: 'Wrong model file',
      sourceUnreadable: 'Page file unavailable',
      exhausted: 'Page edit limit reached',
      existingGeometry: 'Region uses box geometry',
      unconfirmed: 'Written, not shown yet',
      system: 'System refused the request',
    },
    explain: {
      cancelled: 'Nothing was saved. Analyze again when you are ready.',
      unavailable: 'Model review runs only in the desktop app.',
      modelMissing: 'A required model is missing or failed its checksum. Open Models and backends to import or verify it.',
      modelFailed: 'The model stopped before finishing this page. Nothing was written. Try again, or analyze on the CPU.',
      memory: 'SAM-TS-L lettering mask needs about 10 GB of free memory. Close other apps, then analyze again.',
      sourceLimit: 'Review previews are limited to 20 MB and 24 megapixels, in PNG, JPEG, GIF, WebP or BMP.',
      longstrip: 'Text-shaped review needs a paginated chapter. Long strips are not supported yet.',
      undiscovered: 'No lettering components or detector boxes on this page. Nothing will be written.',
      held: 'This component is outside every speech bubble. Turn on Allow components outside speech bubbles to prepare it.',
      stale: 'The page, its lower layers or the plan changed since this preview. Rebuild it before approving.',
      expired: 'The page or the model changed since this analysis. Analyze the page again.',
      badReconstruction: 'Not enough surrounding pixels to rebuild this area. Nothing was written. Add padding or correct the mask, then prepare again.',
      applied: 'The fill is not previewed here. Check it on the page, and use Undo if it looks wrong.',
      cooAbsent: 'The optional COO finder is excluded while its model rights are unresolved. Sound effects rely on the SAM-TS-L lettering mask alone.',
      geometry: 'The page file changed size since it was imported, or it is over 24 megapixels. Nothing was sent.',
      tile: 'Part of this page is too large to upload as one tile. Nothing was sent.',
      invalidRequest: 'The review sent a request this version does not accept. Close the review, open it again and analyze.',
      wrongModel: 'The chosen file is not the pinned model. Nothing was imported. Choose the exact file from the model’s published release.',
      sourceUnreadable: 'The chapter’s source file for this page is missing or cannot be read. Check the chapter’s files, then analyze again.',
      exhausted: 'This page holds too many edits to add another. Nothing was written.',
      existingGeometry: 'An edit with this component’s id already exists with box geometry. Delete it in Layers, then prepare again.',
      unconfirmed: 'The component was written, but the page could not be reloaded. Close and reopen the chapter to see it.',
      system: 'The computer could not supply something the review needs. Nothing was sent or written. Try again.',
    },
    declined: {
      regions: 'Regions only finds detector boxes. Boxes locate text but cannot be written.',
      cpu: 'Analyzed on the CPU. Analyze on WebGPU to write components.',
      jpeg: 'This page is a JPEG. Component writes need a PNG source.',
      format: 'This page’s image format cannot be written. Component writes need a PNG source.',
      indexed: 'This PNG uses an indexed palette, which component writes cannot fill.',
      sub8Bit: 'This PNG has under 8 bits per channel, which component writes cannot fill.',
      host: 'This computer or runtime is not qualified for component writes.',
      provider: 'Part of the model fell back to the CPU during analysis, so this result is review-only.',
      longstrip: 'Component writes need a paginated chapter. Long strips are review-only.',
      box: 'Detector boxes locate text but cannot be written. Select a SAM-TS-L component.',
    },
    correctionReason: {
      empty: 'No write pixels are left. Add pixels or clear corrections.',
      overlap: 'This component overlaps an edit already on the page. Remove that edit, or use the editor’s mask tools.',
      tooLarge: 'The correction is larger than the 16 megapixel plan limit. Clear it and paint a smaller area.',
    },
    failure: {
      prepare: 'Could not prepare the preview. Nothing was written.',
      apply: 'Could not apply the component. Nothing was written.',
      load: 'Could not load this component’s saved correction.',
      models: 'Could not change the installed models.',
      refresh: 'Could not read model readiness.',
    },
    ready: {
      runtime: 'Install ONNX Runtime in Settings, Performance, before analysis.',
      // After it, a `diagnostics.runtime.*` sentence that names the remedy.
      runtimeUnloadable: 'ONNX Runtime is installed but does not load. {reasonKey}.',
      runtimeChecking: 'Checking that ONNX Runtime loads on this computer…',
      runtimeUnchecked: 'Could not check that ONNX Runtime loads on this computer. Press Refresh readiness under Models and backends to check again.',
      rt: 'The Ogkalu comic text & bubble detector profile for this layout is not installed. Import it under Models and backends.',
      ctd: 'Comic Text Detector (CTD) is not installed. Download it in Settings > Models.',
      sam: 'SAM-TS-L lettering mask is not installed. Import both graphs under Models and backends.',
      samUnverified: 'SAM-TS-L lettering mask is not verified yet. Verify it under Models and backends.',
      memory: 'SAM-TS-L lettering mask needs about 10 GB of free memory.',
      backend: 'The selected backend is not available on this computer.',
      cloud: 'The cloud GPU is not ready. Choose another backend, or set it up in Settings > Cloud.',
    },
    dialog: {
      chooseImage: 'Choose a comic page',
      samFolder: 'Select the folder that holds both SAM-TS-L lettering mask ONNX graphs',
      rtFile: 'Select the pinned Ogkalu comic text & bubble detector (Full) detector.onnx graph',
    },
  },
  cloud: {
    configuration: {
      loadFailed: 'Saved cloud configuration could not be read. The deployment will be checked again.',
      saveFailed: 'Cloud configuration changed, but could not be saved. It will be checked again next time.',
      denoiseChanged: 'This deployment’s denoising setup or access needs attention. Its saved cloud configuration has been updated. Check Settings > Cloud, then test the connection again.',
      cleanChanged: 'This deployment’s cleaning setup or access needs attention. Its saved cloud configuration has been updated. Check Settings > Cloud, then test the connection again.',
      cleanMissing: 'This deployment’s cleaning setup or access needs attention. Check Settings > Cloud, then test the connection again.',
    },
    profile: {
      selectFailed: 'The cloud model could not be switched. The one before it is still in use.',
    },
    // Said in all three cloud consents. The first one confirmed in a project
    // stands for the rest of it, for the same endpoint and for every kind of
    // request, so it names what those later requests send and who pays.
    projectConsent: 'Confirming lets this project send to this endpoint without asking again: whole pages for cloud detection and region crops for cloud cleaning. The provider bills your account for each.',
    recovery: {
      check: 'Check saved renders',
      running: 'Waiting for the existing remote job.',
      abandon: 'Abandon attempt',
      duplicateRisk: 'The remote job may still run or finish. Abandoning lets you clean this region again and may cause a duplicate paid render. The journal and cached evidence will be kept.',
      confirmAbandon: 'Accept duplicate risk and abandon',
      keepAttempt: 'Keep attempt',
      actionFailed: 'Recovery action failed. The attempt was kept. Try again.',
      loadError: 'The chapter could not be loaded. Try recovery again after it is available.',
      unresolved: 'An earlier cloud attempt is unresolved. Recover it before cleaning this region again.',
      repairNeeded: 'Repair needed. A saved cloud patch or its inputs changed. The result and recovery evidence have been preserved.',
      gatewayOutOfDate: 'Gateway out of date. Redeploy the gateway to use this app version.',
    },
    analysis: {
      title: 'Cloud GPU',
      disclosure: '{pages} as {tiles}: {pixels} pixels, {bytes} bytes encoded ({size:memory}). The surrounding art is included, not only the lettering.',
      pages: { one: '1 page', other: '{count} pages' },
      tiles: { one: '1 tile', other: '{count} tiles' },
      costUnknown: 'Estimated cost unknown',
      costEstimate: 'Estimated {cost:currency}',
      costNote: 'The provider bills your account for the GPU time used.',
      rights: 'I have the rights to send these page pixels to this provider for analysis.',
      retention: 'I have reviewed and accept this provider’s retention, human review, and training terms for these uploads.',
      reviewOnly: 'Cloud results are for review. They cannot prepare a component write.',
      capabilityMissing: 'This cloud GPU does not offer the selected analysis model. Nothing was sent.',
      stale: 'The page or its visible edits changed. No further tiles were sent.',
      unknown: 'The last tile may have run. It will not be sent again automatically. Check the provider before starting a new analysis.',
      unknownCancelRequested: 'Cancel was requested while a tile was out. That tile may have run. It will not be sent again automatically. Check the provider before starting a new analysis.',
      cancelled: 'Cloud analysis cancelled. Remaining tiles were not sent.',
      capability: {
        label: 'Cloud model',
        sam: '☁ SAM-TS-L lettering mask',
        rt: '☁ Ogkalu comic text & bubble detector (Full)',
        both: '☁ SAM-TS-L lettering mask with Ogkalu comic text & bubble detector (Full)',
        notOffered: '{name}, not offered',
        invalid: 'This cloud GPU’s model list could not be read. Nothing was sent. Update the cloud worker, then try again.',
        notConfigured: 'This cloud GPU has no analysis models set up. Nothing was sent.',
        limits: 'This page needs larger tiles than this cloud GPU accepts. Nothing was sent.',
        modelChanged: 'The cloud GPU’s model changed after you reviewed it. Nothing was sent. Review the page again to see the new model.',
      },
      entry: {
        action: 'Analyze with cloud GPU',
        target: '{name} on {providerKey}',
        note: 'This page only, after you review exactly what is sent.',
        routed: 'Analyze sends this page to {name} after you review exactly what is sent.',
      },
      unavailable: {
        off: 'Cloud engines are off. Turn them on in Settings > Cloud to analyze on your cloud GPU.',
        noTarget: 'To analyze on a cloud GPU, choose a Modal or Beam endpoint in Settings.',
        noSecret: 'The selected cloud endpoint has no stored key. Add it in Settings to analyze on a cloud GPU.',
        secretLocked: 'The system password store is locked, so the cloud key cannot be read. Unlock it, then open the review again.',
        notReady: 'The cloud GPU is not ready. Check the endpoint in Settings > Cloud.',
        longstrip: 'Cloud analysis needs a paginated chapter. This long strip is analyzed on this computer only.',
        settings: 'Open Cloud settings',
      },
      status: {
        checking: 'Checking what {name} offers…',
        preparing: 'Preparing the upload review…',
        progress: '{done} of {total} tiles analyzed on {name}',
        cancelling: 'Cancelling. A tile already sent may still be billed.',
      },
      consent: {
        heading: 'Send page {page} to your cloud GPU?',
        what: 'What is sent',
        where: 'Where it goes',
        whereValue: '{name}, your endpoint on {providerKey}',
        model: 'Model',
        modelValue: '{capabilityKey}, revision {revision}',
        graphs: 'Graph SHA-256 {graphs}',
        eachModel: 'Every tile goes to each model listed.',
        identity: 'Full model identity',
        revision: 'Revision',
        graph: 'Graph SHA-256',
        cost: 'Cost',
        result: 'Result',
        expires: 'This review is valid until {time}.',
        confirm: 'Send to cloud GPU',
        cancel: 'Cancel',
      },
      // Text cleanup with detection on the cloud GPU: one consent per run,
      // for one page, spent when the run starts.
      run: {
        title: 'Detect lettering on your cloud GPU?',
        heading: 'Send page {page} to your cloud GPU, then clean it here?',
        headingChapter: 'Send {pages} of this chapter to your cloud GPU, then clean them here?',
        what: 'What is sent',
        whatValue: '{pages} sent in full for detection, split into {tiles}. {pixels} uploaded pixels, including any overlap. All page artwork is included.',
        where: 'Where it goes',
        models: 'Models',
        result: 'What happens',
        resultValue: 'Your cloud GPU finds the lettering. The page is cleaned on this computer with your Text cleanup settings. If the cloud GPU fails, the page is left as it was.',
        scope: 'This page only.',
        scopeChapter: 'Only these pages, for this run. A page that fails is left as it was and the run goes on.',
        confirm: 'Send and clean',
        consentFailed: 'Could not confirm the cloud upload. Nothing was sent. Try again.',
        gate: 'Detection is set to run on your cloud GPU, and it is not ready.',
        unavailable: 'Detection is set to run on your cloud GPU, and it is not ready. Nothing was sent. Open Settings > Cloud, or choose This computer for Detect on in Text cleanup.',
        scopeProject: 'Cloud detection cannot run on a whole project: each chapter is planned on its own. Run one chapter at a time, or choose This computer for Detect on in Text cleanup.',
        tooMany: 'This chapter has more pages than one cloud run may send. Nothing was sent. Run it a page at a time, or choose This computer for Detect on in Text cleanup.',
        tooLargeChapter: 'This chapter has more pixels than one cloud run may send. Nothing was sent. Run it a page at a time, or choose This computer for Detect on in Text cleanup.',
        nothing: 'Every page in scope is already cleaned. Nothing was sent.',
        stopped: 'Cloud detection stopped after page {page} ({code}). The pages after it were not sent and are left for a later run.',
        consentLost: 'The cloud consent no longer matches this run. Nothing was sent. Start the run again.',
        expired: 'The cloud consent expired before the run started. Nothing was sent. Start the run again.',
        profileChanged: 'The cloud endpoint changed after you confirmed. Nothing further was sent. Start the run again.',
        tooLarge: 'This page is larger than your cloud GPU accepts. Nothing was sent. Choose This computer for Detect on in Text cleanup.',
        pageFailed: 'Page {page} was not cleaned: cloud detection failed ({code}). The page is unchanged.',
      },
      progressLabel: 'Cloud analysis progress',
      cancel: 'Cancel cloud analysis',
      source: 'Cloud evidence from {name}',
      provenance: '{capabilityKey} on {name}, {providerKey}',
      noImage: 'This result has no page image. Its boxes and components are drawn on a blank page.',
      outcome: {
        cloudDisabled: 'Cloud engines are off',
        cloudProfile: 'Cloud endpoint unavailable',
        capabilityMissing: 'Model not offered',
        consentRequired: 'Consent needed',
        proposalExpired: 'Upload review expired',
        proposalConsumed: 'Upload review already used',
        proposalLimit: 'Too many upload reviews open',
        cancelled: 'Cloud analysis cancelled',
        stale: 'Page changed during analysis',
        unknown: 'Last tile state unknown',
        invalid: 'Unusable cloud result',
      },
      explain: {
        cloudDisabled: 'Allow cloud engines in Settings to analyze on a cloud GPU. Nothing was sent.',
        proposalExpired: 'An upload review is valid for 5 minutes. Nothing was sent. Review the page again.',
        proposalConsumed: 'This upload review was already used. Review the page again to start a new analysis.',
        proposalLimit: 'Finish or cancel other cloud analyses first. Nothing was sent.',
        upload: 'This page needs more tiles than one cloud analysis may send. Nothing was sent.',
        invalid: 'The cloud GPU returned a result this app cannot use. Nothing was added to the review. Tiles already analyzed may be billed.',
      },
      profile: {
        inactive: 'The selected cloud endpoint changed. Nothing was sent. Choose it again in Settings.',
        missing: 'This cloud endpoint no longer exists. Nothing was sent.',
        credential: 'This cloud endpoint has no usable key. Nothing was sent. Add the key in Settings.',
        config: 'This cloud endpoint’s settings are invalid. Nothing was sent. Check it in Settings.',
      },
      consentRequired: {
        rights: 'Confirm that you have the rights to send these page pixels. Nothing was sent.',
        retention: 'Accept the provider’s retention and training terms first. Nothing was sent.',
      },
      failure: {
        capabilities: 'Could not read what this cloud GPU offers. Nothing was sent.',
        propose: 'Could not prepare the upload review. Nothing was sent.',
        confirm: 'The cloud analysis stopped. Nothing was added to the review. Tiles already sent may be billed.',
      },
    },
    // Cleaning detected regions on the cloud GPU: one consent for the exact
    // regions, sent in bounded batches. Preparing cleans nothing.
    clean: {
      title: 'Clean on your cloud GPU?',
      heading: 'Clean {regions} on your cloud GPU?',
      regions: { one: '1 region', other: '{count} regions' },
      whatValue: '{regions} from {pages}: {pixels} pixels, cropped around each region. Whole pages are not sent.',
      execution: 'Cleaned on',
      executionCloud: 'Your cloud GPU only. Nothing is cleaned on this computer.',
      executionMixed: {
        zero: 'No region here is a flat colour, so every region goes to your cloud GPU.',
        one: 'After you start, 1 flat-colour region is tried on this computer first. If it cleans here, it is not sent. The GPU time and cost range below still count it as sent, so you agree to pay up to that full amount.',
        other: 'After you start, {count} flat-colour regions are tried on this computer first. Those that clean here are not sent. The GPU time and cost range below still count every region as sent, so you agree to pay up to that full amount.',
      },
      // The explicit mixed choice, as the Text cleanup tool names it.
      mixed: {
        label: 'Clean flat colours on this computer first',
      },
      batches: 'Sent in',
      batchCount: { one: '1 batch', other: '{count} batches' },
      batchesValue: '{batches} of up to {size} regions, one after another.',
      batchesNote: 'This one consent covers every batch. Before each batch your cloud setup, key and GPU are checked again; if any has changed, or you cancel, the batches not yet sent stop and their regions stay detected.',
      heldBack: 'Left out',
      heldBackValue: {
        one: '1 region is not included: an earlier cloud request for it has not been resolved. Recover it first.',
        other: '{count} regions are not included: an earlier cloud request for each has not been resolved. Recover them first.',
      },
      tooLarge: 'Too large',
      tooLargeValue: {
        one: '1 region is not included: it is too large for your cloud GPU to take in one piece. Choose This computer under Clean on to clean it.',
        other: '{count} regions are not included: each is too large for your cloud GPU to take in one piece. Choose This computer under Clean on to clean them.',
      },
      gpu: 'GPU',
      gpuUnknown: 'Not stated by the endpoint',
      costRange: 'Estimated {low:currency} to {high:currency}',
      gpuTime: 'About {low} to {high} minutes of GPU time, including start-up and idle time',
      costBasis: 'Estimated from each region’s own crop size. Your provider bills actual use.',
      resultValue: 'Each region is cleaned on your cloud GPU and saved as it arrives. A region that fails stays detected. It is not cleaned on this computer instead.',
      scope: 'Only these regions.',
      plan: 'Plan fingerprint',
      asksAfterDetection: 'Your cloud GPU finds the lettering and the regions are saved as detected. When detection finishes, the regions it found are cleaned on your cloud GPU too.',
      confirm: 'Clean on cloud GPU',
      declined: 'Nothing was sent. The regions stay detected, ready to clean.',
      unresolvedOnly: {
        one: 'Nothing was sent. The 1 region here has an earlier cloud request that has not been resolved. Recover it first.',
        other: 'Nothing was sent. The {count} regions here have earlier cloud requests that have not been resolved. Recover them first.',
      },
      regionTooLarge: 'This region is too large for your cloud GPU to take in one piece. Nothing was sent. Clean it on this computer instead.',
      tooLargeOnly: {
        one: 'Nothing was sent. The 1 region here is too large for your cloud GPU to take in one piece. Choose This computer under Clean on to clean it.',
        other: 'Nothing was sent. The {count} regions here are too large for your cloud GPU to take in one piece. Choose This computer under Clean on to clean them.',
      },
      nothing: 'No detected regions are waiting here. Nothing was sent.',
      gate: 'Cleaning is set to run on your cloud GPU, and it is not ready.',
      unavailable: 'Cleaning is set to run on your cloud GPU, and it is not ready. Nothing was sent. Open Settings > Cloud, or choose This computer under Clean on.',
      scopeProject: 'Cloud cleaning cannot run on a whole project: each chapter is planned on its own. Run one chapter at a time, or choose This computer under Clean on.',
      busy: 'Another run is going. Nothing was sent. The regions stay detected.',
      leftDetected: 'The chapter changed before cleaning started. Its regions stay detected. Clean them later with Step set to Clean.',
      consentLost: 'The chapter changed after you confirmed. Nothing was sent. The regions stay detected.',
      expired: 'The cloud consent expired before cleaning started. Nothing was sent. Try again.',
      profileChanged: 'Your cloud setup changed after you confirmed. Nothing was sent. Try again.',
      // The native commands' refusals (`editor/cloudrun.js#cloudCleanErrorKey`).
      error: {
        notReady: 'Your cloud GPU is not set up for cleaning. Nothing was sent. Check Settings > Cloud.',
        scope: 'Cloud cleaning cannot run on this selection. Nothing was sent.',
        gone: 'The regions changed before they could be prepared. Nothing was sent. Try again.',
        sourceMissing: 'The source page is missing. Nothing was sent. Restore the page and try again.',
        tooMany: 'Too many regions for one cloud clean. Nothing was sent. Clean part of the chapter at a time.',
        proposalLimit: 'Too many cloud cleans are waiting for an answer. Nothing was sent. Try again in a few minutes.',
        unauthorized: 'Your cloud GPU refused the app’s key. Nothing was sent. Check Settings > Cloud.',
        unreachable: 'Your cloud GPU could not be reached. Nothing was sent. Check your connection and try again.',
        gateway: 'Your cloud GPU answered with an error. Nothing was sent. Try again later.',
        statements: 'Both statements must be checked before anything is sent. Nothing was sent.',
        consentGone: 'That consent is no longer open. Nothing was sent. Try again.',
        planChanged: 'The plan you confirmed is not the one prepared. Nothing was sent. Try again.',
      },
      failure: {
        prepare: 'Could not prepare the cloud clean. Nothing was sent. The regions stay detected.',
        confirm: 'Could not confirm the cloud clean. Nothing was sent. The regions stay detected.',
        start: 'The cloud clean did not start. Nothing was sent. The regions stay detected.',
      },
    },
    // The cloud GPU as a row of the bottom-left resource panel
    // (`state/cloudgpu.svelte.js`). The GPU name is the provider's (`L4`), a value.
    gpu: {
      title: 'Running in the cloud',
      titleMixed: 'Using memory and a cloud GPU',
      name: {
        render: 'Cloud GPU · {gpu}',
        analysis: 'Cloud GPU · {gpu} (analysis)',
      },
      state: {
        starting: 'Starting',
        busy: 'Busy',
        idle: 'Idle',
        // The provider scales an idle GPU down on its own; the time is its
        // estimate from the last activity, not a promise.
        idleStops: 'Idle, stops in ~{time}',
        stopping: 'Stopping…',
        // The last status read failed; the row is what the read before it said.
        unknown: 'Status unavailable, retrying…',
      },
      // The list price, beside the row where a local model shows its memory.
      price: '~{price:currency}/h',
      priceNote: 'List price estimate: about {price:currency} an hour while this GPU is up. The provider bills the time actually used.',
      noPrice: 'The provider bills this GPU for the time it is up.',
      action: {
        stop: 'Stop cloud GPU',
        stopAnalysis: 'Stop cloud GPU (analysis)',
      },
      confirm: {
        title: 'Stop the cloud GPU?',
        // Each stop ends one container, and only its own work.
        bodyRender: 'Cloud renders that are still running will be cancelled. Cloud analysis is not affected. A render already sent may still be billed.',
        bodyAnalysis: 'Cloud analysis that is still running will be cancelled. Cloud renders are not affected. A tile already sent may still be billed.',
        action: 'Stop GPU',
      },
      notice: {
        stopFailed: 'The cloud GPU could not be stopped. It still stops by itself after its idle time.',
      },
    },
  },
}
