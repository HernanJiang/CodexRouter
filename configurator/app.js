const { createApp, reactive, toRefs } = Vue;

const KNOWN_REASONING = {
  'gpt-5.6-sol': { levels: ['low','medium','high','xhigh','max','ultra'], default: 'low', fast: true },
  'gpt-5.6-terra': { levels: ['low','medium','high','xhigh','max','ultra'], default: 'medium', fast: true },
  'gpt-5.6-luna': { levels: ['low','medium','high','xhigh','max'], default: 'medium', fast: true },
  'grok-4.5': { levels: ['minimal','low','medium','high','xhigh'], default: 'medium', fast: false },
  'deepseek-v4-flash': { levels: ['minimal','low','medium','high','xhigh'], default: 'low', fast: false },
  'deepseek-v4': { levels: ['minimal','low','medium','high','xhigh'], default: 'medium', fast: false },
  'kimi-coding': { levels: [], default: '', fast: false },
  'claude-opus-5': { levels: [], default: '', fast: false }
};

const KNOWN_MULTIMODAL = [
  'gpt-4o', 'gpt-4.5', 'gpt-5', 'gpt-5.6', 'claude-3', 'claude-opus', 'claude-sonnet',
  'gemini', 'kimi', 'k3', 'grok-3', 'grok-4', 'qwen', 'qwen2', 'qwen2.5', 'llava',
  'yi-vision', 'internvl', 'minicpm', 'glm-4', 'glm4'
];

function detectMultimodal(modelName) {
  const mn = modelName.toLowerCase();
  return KNOWN_MULTIMODAL.some(marker => mn.includes(marker));
}

function resolveMultimodal(m) {
  const val = m.multimodal;
  if (val === true || val === 'true') return true;
  if (val === false || val === 'false') return false;
  return detectMultimodal(m.model);
}

function detectReasoning(modelName) {
  const key = Object.keys(KNOWN_REASONING).find(k => modelName.toLowerCase().includes(k.toLowerCase()));
  return key ? KNOWN_REASONING[key] : { levels: [], default: '', fast: false };
}

function defaultModel() {
  return { model: '', alias: '', baseURL: '', apiKey: '', priority: 10, weight: 1, extra: '{}', multimodal: 'auto' };
}

function guessProviderId(prefix, model, idx) {
  const slug = model ? model.replace(/[^a-zA-Z0-9_-]/g, '-') : 'model-' + (idx+1);
  return prefix + '-' + slug;
}

createApp({
  setup() {
    const state = reactive({
      models: [defaultModel()],
      authMode: 'chatgpt_oauth',
      localApiKey: '',
      oauthFallback: { enabled: true, officialPriority: 1, fallbackPriority: 100 },
      reasoning: { mode: 'auto', levels: '', defaultLevel: '', supportsFast: false },
      deploy: {
        codexHome: '',
        sub2apiHost: 'http://127.0.0.1:18080',
        ccSwitchDb: '',
        providerPrefix: 'codex-router',
        generateIsolation: true
      },
      outputs: []
    });

    function addModel() { state.models.push(defaultModel()); }
    function removeModel(idx) { state.models.splice(idx, 1); }

    function getReasoningForModel(m) {
      if (state.reasoning.mode === 'manual') {
        const levels = state.reasoning.levels.split(',').map(s => s.trim()).filter(Boolean);
        return { levels, default: state.reasoning.defaultLevel || (levels[0] || ''), fast: state.reasoning.supportsFast };
      }
      return detectReasoning(m.model);
    }

    function buildModelCatalog() {
      return state.models.map((m, idx) => {
        const r = getReasoningForModel(m);
        const multimodal = resolveMultimodal(m);
        return {
          slug: m.model,
          display_name: m.alias || m.model,
          supports_vision: multimodal,
          description: 'Configured model #' + (idx+1),
          default_reasoning_level: r.default,
          supported_reasoning_levels: r.levels.map(effort => ({ effort, description: effort + ' reasoning level' })),
          shell_type: 'shell_command',
          visibility: 'list',
          supported_in_api: true,
          priority: m.priority,
          additional_speed_tiers: r.fast ? ['fast'] : [],
          service_tiers: r.fast ? [{ id: 'priority', name: 'Fast', description: 'Faster responses with higher usage' }] : []
        };
      });
    }

    function buildCCSwitchProviders() {
      return state.models.map((m, idx) => {
        const provider = {
          id: guessProviderId(state.deploy.providerPrefix, m.model, idx),
          name: (m.alias || m.model) + ' (Codex Router)',
          app_type: 'codex',
          settings: {
            model_provider: 'sub2api',
            model: m.model,
            api_url: state.deploy.sub2apiHost + '/v1'
          }
        };
        if (state.authMode === 'chatgpt_oauth') {
          provider.settings.api_key = '<YOUR_SUB2API_KEY>';
          provider.settings.requires_openai_auth = true;
        } else {
          provider.settings.api_key = state.localApiKey || '<YOUR_LOCAL_API_KEY>';
          provider.settings.requires_openai_auth = false;
        }
        return provider;
      });
    }

    function buildSub2ApiChannels() {
      return state.models.map(m => {
        let extra = {};
        try { extra = JSON.parse(m.extra || '{}'); } catch {}
        return {
          name: m.alias || m.model,
          type: 'openai',
          base_url: m.baseURL,
          key: m.apiKey,
          models: [m.model],
          priority: m.priority,
          weight: m.weight,
          supports_vision: resolveMultimodal(m),
          ...extra
        };
      });
    }

    function buildUnifiedConfig() {
      return {
        version: '0.2.0',
        authMode: state.authMode,
        localApiKey: state.localApiKey,
        deploy: {
          codexHome: state.deploy.codexHome,
          sub2apiHost: state.deploy.sub2apiHost,
          ccSwitchDb: state.deploy.ccSwitchDb,
          generateIsolation: state.deploy.generateIsolation
        },
        oauthFallback: state.oauthFallback,
        reasoning: state.reasoning.mode === 'manual' ? {
          mode: 'manual',
          levels: state.reasoning.levels.split(',').map(s => s.trim()).filter(Boolean),
          defaultLevel: state.reasoning.defaultLevel,
          supportsFast: state.reasoning.supportsFast
        } : { mode: 'auto' },
        models: state.models,
        modelCatalog: buildModelCatalog(),
        ccSwitchProviders: state.deploy.generateIsolation ? buildCCSwitchProviders() : []
      };
    }

    function generate() {
      const cfg = buildUnifiedConfig();
      state.outputs = [
        { name: 'codex-router-config.json', content: JSON.stringify(cfg, null, 2) },
        { name: 'sub2api-channels.json', content: JSON.stringify(buildSub2ApiChannels(), null, 2) },
        { name: 'cc-switch-providers.json', content: JSON.stringify(cfg.ccSwitchProviders, null, 2) }
      ];
    }

    function downloadAll() {
      generate();
      state.outputs.forEach(out => {
        const blob = new Blob([out.content], { type: 'application/octet-stream' });
        const url = URL.createObjectURL(blob);
        const a = document.createElement('a');
        a.href = url;
        a.download = out.name;
        a.click();
        URL.revokeObjectURL(url);
      });
    }

    async function copy(text) {
      try {
        await navigator.clipboard.writeText(text);
        alert('已复制到剪贴板');
      } catch {
        alert('复制失败，请手动复制');
      }
    }

    function loadExample() {
      state.models = [
        { model: 'gpt-5.6-sol', alias: 'GPT-5.6 Sol', baseURL: 'https://api.openai.com/v1', apiKey: '', priority: 1, weight: 1, extra: '{}', multimodal: 'auto' },
        { model: 'deepseek-v4-flash', alias: 'DeepSeek V4 Flash', baseURL: 'https://openrouter.ai/api/v1', apiKey: '', priority: 10, weight: 1, extra: '{}', multimodal: 'auto' }
      ];
      state.authMode = 'chatgpt_oauth';
      state.localApiKey = '';
      state.oauthFallback = { enabled: true, officialPriority: 1, fallbackPriority: 100 };
      state.reasoning = { mode: 'auto', levels: '', defaultLevel: '', supportsFast: false };
      state.deploy.codexHome = '';
      state.deploy.sub2apiHost = 'http://127.0.0.1:18080';
    }

    return {
      ...toRefs(state),
      addModel, removeModel, generate, downloadAll, copy, loadExample
    };
  }
}).mount('#app');
