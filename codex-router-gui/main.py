#!/usr/bin/env python3
import json
import os
import re
import shutil
import sqlite3
import subprocess
import sys
import tkinter as tk
from pathlib import Path
from tkinter import filedialog, messagebox, ttk

KNOWN_REASONING = {
    'gpt-5.6-sol': {'levels': ['low','medium','high','xhigh','max','ultra'], 'default': 'low', 'fast': True},
    'gpt-5.6-terra': {'levels': ['low','medium','high','xhigh','max','ultra'], 'default': 'medium', 'fast': True},
    'gpt-5.6-luna': {'levels': ['low','medium','high','xhigh','max'], 'default': 'medium', 'fast': True},
    'grok-4.5': {'levels': ['minimal','low','medium','high','xhigh'], 'default': 'medium', 'fast': False},
    'deepseek-v4-flash': {'levels': ['minimal','low','medium','high','xhigh'], 'default': 'low', 'fast': False},
    'deepseek-v4': {'levels': ['minimal','low','medium','high','xhigh'], 'default': 'medium', 'fast': False},
    'kimi-coding': {'levels': [], 'default': '', 'fast': False},
    'claude-opus-5': {'levels': [], 'default': '', 'fast': False},
}

KNOWN_MULTIMODAL = [
    'gpt-4o', 'gpt-4.5', 'gpt-5', 'gpt-5.6', 'claude-3', 'claude-opus', 'claude-sonnet',
    'gemini', 'kimi', 'k3', 'grok-3', 'grok-4', 'qwen', 'qwen2', 'qwen2.5', 'llava',
    'yi-vision', 'internvl', 'minicpm', 'glm-4', 'glm4'
]


def detect_reasoning(model_name):
    key = next((k for k in KNOWN_REASONING if k.lower() in model_name.lower()), None)
    return KNOWN_REASONING[key] if key else {'levels': [], 'default': '', 'fast': False}


def detect_multimodal(model_name):
    mn = model_name.lower()
    return any(marker in mn for marker in KNOWN_MULTIMODAL)


def resource_path(relative):
    if hasattr(sys, '_MEIPASS'):
        return os.path.join(sys._MEIPASS, relative)
    return os.path.join(os.path.dirname(__file__), relative)


def find_router_root():
    exe_dir = Path(sys.executable).parent if getattr(sys, 'frozen', False) else Path(__file__).resolve().parent.parent
    if (exe_dir / 'scripts' / 'Start-Router.ps1').exists():
        return exe_dir
    # try parent
    parent = exe_dir.parent
    if (parent / 'scripts' / 'Start-Router.ps1').exists():
        return parent
    return exe_dir


class WizardPage(ttk.Frame):
    def __init__(self, master, app, title, description, **kwargs):
        super().__init__(master, **kwargs)
        self.app = app
        ttk.Label(self, text=title, font=('Microsoft YaHei', 16, 'bold')).pack(anchor='w', pady=(0, 8))
        ttk.Label(self, text=description, wraplength=720, foreground='#64748b').pack(anchor='w', pady=(0, 20))
        self.content = ttk.Frame(self)
        self.content.pack(fill='both', expand=True)
        self.buttons = ttk.Frame(self)
        self.buttons.pack(fill='x', side='bottom', pady=16)


class CodexRouterApp:
    def __init__(self, root):
        self.root = root
        self.root.title('Codex-Router Configurator')
        self.root.geometry('880x720')
        self.root.minsize(780, 620)
        self._set_theme()

        self.router_root = find_router_root()
        self.config_path = self.router_root / 'codex-router-config.json'
        self.state = self._default_state()
        self.pages = []
        self.current_page = 0

        if self.config_path.exists():
            self.show_main_dashboard()
        else:
            self.show_wizard()

    def _default_state(self):
        return {
            'version': '0.3.0',
            'authMode': 'chatgpt_oauth',
            'localApiKey': '',
            'deploy': {
                'codexHome': '',
                'sub2apiHost': 'http://127.0.0.1:18080',
                'ccSwitchDb': '',
                'generateIsolation': True,
            },
            'oauthFallback': {'enabled': True, 'officialPriority': 1, 'fallbackPriority': 100},
            'reasoning': {'mode': 'auto', 'levels': [], 'defaultLevel': '', 'supportsFast': False},
            'proxy': {'enabled': False, 'type': 'http', 'host': '127.0.0.1', 'port': '7890', 'username': '', 'password': ''},
            'models': [],
            'modelCatalog': [],
            'ccSwitchProviders': [],
        }

    def _set_theme(self):
        style = ttk.Style()
        try:
            style.theme_use('vista')
        except tk.TclError:
            pass
        style.configure('TFrame', background='#f8fafc')
        style.configure('TLabel', background='#f8fafc', font=('Microsoft YaHei', 10))
        style.configure('TButton', font=('Microsoft YaHei', 10))
        style.configure('TCheckbutton', background='#f8fafc', font=('Microsoft YaHei', 10))
        style.configure('TRadiobutton', background='#f8fafc', font=('Microsoft YaHei', 10))
        style.configure('Primary.TButton', font=('Microsoft YaHei', 10, 'bold'))

    def _entry_var(self, parent, label, default='', password=False, width=50):
        f = ttk.Frame(parent)
        f.pack(fill='x', pady=6)
        ttk.Label(f, text=label, width=18, anchor='w').pack(side='left')
        var = tk.StringVar(value=default)
        ent = ttk.Entry(f, textvariable=var, width=width, show='*' if password else '')
        ent.pack(side='left', fill='x', expand=True, padx=(8, 0))
        return var

    def show_wizard(self):
        for w in self.root.winfo_children():
            w.destroy()
        self.wizard_container = ttk.Frame(self.root)
        self.wizard_container.pack(fill='both', expand=True, padx=20, pady=20)

        self.pages = [
            self._build_welcome_page(),
            self._build_project_page(),
            self._build_auth_page(),
            self._build_model_page(),
            self._build_proxy_page(),
            self._build_finish_page(),
        ]
        self.current_page = 0
        self._show_page(0)

    def _nav_buttons(self, page, show_back=True, next_text='下一步', next_cmd=None):
        if show_back:
            ttk.Button(page.buttons, text='上一步', command=self._prev_page).pack(side='left', padx=4)
        ttk.Button(page.buttons, text=next_text, command=next_cmd or self._next_page).pack(side='right', padx=4)

    def _build_welcome_page(self):
        p = WizardPage(self.wizard_container, self, '欢迎使用 Codex-Router',
                       '本向导会一步一步帮你配置第一个模型，并自动完成 Codex、CC Switch 与代理的设置。点击「开始配置」继续。')
        ttk.Label(p.content, text='你将需要：', font=('Microsoft YaHei', 11, 'bold')).pack(anchor='w', pady=(20, 8))
        for t in ['Sub2API 项目目录（建议把本 EXE 放在项目根目录）', '至少一个模型的 API Key 与 Base URL', '（可选）CC Switch 数据库路径']:
            ttk.Label(p.content, text='  • ' + t).pack(anchor='w', pady=2)
        self._nav_buttons(p, show_back=False, next_text='开始配置')
        return p

    def _build_project_page(self):
        p = WizardPage(self.wizard_container, self, '选择项目目录',
                       '请选择或确认 Codex Router 项目根目录。本目录应包含 scripts/Start-Router.ps1。')
        f = ttk.Frame(p.content)
        f.pack(fill='x', pady=20)
        self.project_var = tk.StringVar(value=str(self.router_root))
        ttk.Entry(f, textvariable=self.project_var, width=60).pack(side='left', fill='x', expand=True, padx=(0, 8))
        ttk.Button(f, text='浏览...', command=self._browse_project).pack(side='left')
        self.project_status = ttk.Label(p.content, text='', foreground='#dc2626')
        self.project_status.pack(anchor='w', pady=8)
        self._nav_buttons(p, next_cmd=self._validate_project)
        return p

    def _browse_project(self):
        d = filedialog.askdirectory()
        if d:
            self.project_var.set(d)

    def _validate_project(self):
        path = Path(self.project_var.get())
        if not (path / 'scripts' / 'Start-Router.ps1').exists():
            self.project_status.config(text='未找到 scripts/Start-Router.ps1，请确认项目根目录')
            return
        self.router_root = path
        self.config_path = self.router_root / 'codex-router-config.json'
        self.project_status.config(text='')
        self._next_page()

    def _build_auth_page(self):
        p = WizardPage(self.wizard_container, self, '选择 Codex 登录方式',
                       '两种方式最终都通过本机 Sub2API 路由模型请求，效果一致。')
        self.wizard_auth_mode = tk.StringVar(value='chatgpt_oauth')
        ttk.Radiobutton(p.content, text='使用 ChatGPT 账号登录 Codex（推荐，与官方体验一致）',
                        variable=self.wizard_auth_mode, value='chatgpt_oauth').pack(anchor='w', pady=8)
        ttk.Radiobutton(p.content, text='使用本地 API Key 直接登录（无需 ChatGPT 账号）',
                        variable=self.wizard_auth_mode, value='local_api_key').pack(anchor='w', pady=8)
        self.wizard_local_key = self._entry_var(p.content, '本地 API Key', password=True)
        self._nav_buttons(p, next_cmd=self._save_auth_page)
        return p

    def _save_auth_page(self):
        self.state['authMode'] = self.wizard_auth_mode.get()
        self.state['localApiKey'] = self.wizard_local_key.get()
        self._next_page()

    def _build_model_page(self):
        p = WizardPage(self.wizard_container, self, '配置第一个模型',
                       '填写你最常用的模型。配置完成后可随时在高级设置中添加更多模型。')
        self.wizard_model_name = self._entry_var(p.content, '模型名称', 'deepseek-v4-flash')
        self.wizard_model_alias = self._entry_var(p.content, '显示别名', 'DeepSeek V4 Flash')
        self.wizard_model_baseurl = self._entry_var(p.content, 'Base URL', 'https://openrouter.ai/api/v1')
        self.wizard_model_key = self._entry_var(p.content, 'API Key', password=True)
        self._nav_buttons(p, next_cmd=self._save_model_page)
        return p

    def _save_model_page(self):
        name = self.wizard_model_name.get().strip()
        if not name:
            messagebox.showerror('输入错误', '模型名称不能为空')
            return
        model = {
            'model': name,
            'alias': self.wizard_model_alias.get().strip() or name,
            'baseURL': self.wizard_model_baseurl.get().strip(),
            'apiKey': self.wizard_model_key.get(),
            'priority': 10,
            'weight': 1,
            'extra': '{}',
            'multimodal': 'auto'
        }
        self.state['models'] = [model]
        self.state['deploy']['sub2apiHost'] = 'http://127.0.0.1:18080'
        self._next_page()

    def _build_proxy_page(self):
        p = WizardPage(self.wizard_container, self, '网络代理（可选）',
                       '如果你使用 Clash、V2Ray、SSR 等梯子，可以在这里开启代理。')
        self.wizard_proxy_enabled = tk.BooleanVar(value=False)
        ttk.Checkbutton(p.content, text='启用网络代理', variable=self.wizard_proxy_enabled).pack(anchor='w', pady=8)
        self.wizard_proxy_type = ttk.Combobox(p.content, values=['http', 'https', 'socks5', 'socks5h'], state='readonly', width=15)
        self.wizard_proxy_type.set('http')
        self.wizard_proxy_type.pack(anchor='w', pady=4)
        self.wizard_proxy_host = self._entry_var(p.content, '代理地址', '127.0.0.1')
        self.wizard_proxy_port = self._entry_var(p.content, '代理端口', '7890')
        self.wizard_ccsync = tk.BooleanVar(value=False)
        ttk.Checkbutton(p.content, text='同时同步到 CC Switch 数据库（需填写 CC Switch DB 路径）', variable=self.wizard_ccsync).pack(anchor='w', pady=(12, 4))
        self._nav_buttons(p, next_cmd=self._save_proxy_page)
        return p

    def _save_proxy_page(self):
        self.state['proxy'] = {
            'enabled': self.wizard_proxy_enabled.get(),
            'type': self.wizard_proxy_type.get(),
            'host': self.wizard_proxy_host.get().strip(),
            'port': self.wizard_proxy_port.get().strip(),
            'username': '',
            'password': ''
        }
        self.state['deploy']['ccSwitchSync'] = self.wizard_ccsync.get()
        self._next_page()

    def _build_finish_page(self):
        p = WizardPage(self.wizard_container, self, '完成配置',
                       '点击「一键完成配置」，程序会自动写入 Codex 配置、生成代理启动脚本。若你在下方启用了 CC Switch 同步，还会自动写入 CC Switch Provider。')
        self.finish_label = ttk.Label(p.content, text='准备就绪', wraplength=720)
        self.finish_label.pack(anchor='w', pady=20)
        self.progress = ttk.Progressbar(p.content, mode='indeterminate', length=400)
        self._nav_buttons(p, next_text='一键完成配置', next_cmd=self._apply_all)
        return p

    def _show_page(self, idx):
        for i, p in enumerate(self.pages):
            p.pack_forget() if i != idx else None
        self.pages[idx].pack(fill='both', expand=True)
        self.current_page = idx

    def _next_page(self):
        if self.current_page < len(self.pages) - 1:
            self._show_page(self.current_page + 1)

    def _prev_page(self):
        if self.current_page > 0:
            self._show_page(self.current_page - 1)

    def _apply_all(self):
        self.progress.pack(fill='x', pady=12)
        self.progress.start()
        self.finish_label.config(text='正在应用配置，请稍候...')
        self.root.update()
        try:
            self._build_derived_config()
            self._write_all_files()
            self._write_codex_config()
            if self.state.get('deploy', {}).get('ccSwitchSync'):
                self._sync_ccswitch()
            self._write_proxy_script()
            self.progress.stop()
            self.progress.pack_forget()
            messagebox.showinfo('配置完成', '所有配置已自动应用。\n\n现在可以点击「启动路由」开始运行。')
            self.show_main_dashboard()
        except Exception as e:
            self.progress.stop()
            self.progress.pack_forget()
            messagebox.showerror('配置失败', str(e))
            self.finish_label.config(text='配置失败：' + str(e))

    def _build_derived_config(self):
        catalog = []
        providers = []
        prefix = 'codex-router'
        for i, m in enumerate(self.state['models']):
            r = detect_reasoning(m['model']) if self.state['reasoning']['mode'] == 'auto' else {
                'levels': self.state['reasoning'].get('levels', []),
                'default': self.state['reasoning'].get('defaultLevel', ''),
                'fast': self.state['reasoning'].get('supportsFast', False)
            }
            multimodal = self._resolve_multimodal(m)
            catalog.append({
                'slug': m['model'],
                'display_name': m['alias'] or m['model'],
                'description': f'Configured model #{i+1}',
                'supports_vision': multimodal,
                'default_reasoning_level': r['default'],
                'supported_reasoning_levels': [{'effort': e, 'description': f'{e} reasoning level'} for e in r['levels']],
                'shell_type': 'shell_command',
                'visibility': 'list',
                'supported_in_api': True,
                'priority': m['priority'],
                'additional_speed_tiers': ['fast'] if r['fast'] else [],
                'service_tiers': [{'id': 'priority', 'name': 'Fast', 'description': 'Faster responses with higher usage'}] if r['fast'] else []
            })
            settings = {
                'model_provider': 'sub2api',
                'model': m['model'],
                'api_url': self.state['deploy']['sub2apiHost'].rstrip('/') + '/v1',
            }
            if self.state['authMode'] == 'chatgpt_oauth':
                settings['api_key'] = '<YOUR_SUB2API_KEY>'
                settings['requires_openai_auth'] = True
            else:
                settings['api_key'] = self.state['localApiKey'] or '<YOUR_LOCAL_API_KEY>'
                settings['requires_openai_auth'] = False
            providers.append({
                'id': f"{prefix}-{re.sub(r'[^a-zA-Z0-9_-]', '-', m['model'])}",
                'name': f"{m['alias'] or m['model']} (Codex Router)",
                'app_type': 'codex',
                'settings': settings
            })
        self.state['modelCatalog'] = catalog
        self.state['ccSwitchProviders'] = providers if self.state['deploy']['generateIsolation'] else []

    def _write_all_files(self):
        self.config_path.write_text(json.dumps(self.state, indent=2, ensure_ascii=False), encoding='utf-8')
        config_dir = self.router_root / 'config'
        config_dir.mkdir(exist_ok=True)
        (config_dir / 'model-catalog.json').write_text(json.dumps(self.state['modelCatalog'], indent=2, ensure_ascii=False), encoding='utf-8')
        (config_dir / 'cc-switch-providers.json').write_text(json.dumps(self.state['ccSwitchProviders'], indent=2, ensure_ascii=False), encoding='utf-8')
        (config_dir / 'sub2api-channels.json').write_text(json.dumps(self._build_sub2api_channels(), indent=2, ensure_ascii=False), encoding='utf-8')

    def _resolve_multimodal(self, m):
        val = m.get('multimodal', 'auto')
        if val is True or val == 'true':
            return True
        if val is False or val == 'false':
            return False
        return detect_multimodal(m.get('model', ''))

    def _build_sub2api_channels(self):
        channels = []
        for m in self.state['models']:
            extra = {}
            try:
                extra = json.loads(m.get('extra') or '{}')
            except json.JSONDecodeError:
                pass
            channels.append({
                'name': m['alias'] or m['model'],
                'type': 'openai',
                'base_url': m['baseURL'],
                'key': m['apiKey'],
                'models': [m['model']],
                'priority': m['priority'],
                'weight': m['weight'],
                'supports_vision': self._resolve_multimodal(m),
                **extra
            })
        return channels

    def _write_codex_config(self):
        codex_home = Path(self.state['deploy']['codexHome']) if self.state['deploy']['codexHome'] else Path.home() / '.codex'
        codex_home.mkdir(parents=True, exist_ok=True)
        config_file = codex_home / 'config.toml'
        first = self.state['models'][0] if self.state['models'] else {'model': 'deepseek-v4-flash'}
        requires = 'false' if self.state['authMode'] == 'local_api_key' else 'true'
        api_key = self.state['localApiKey'] if self.state['authMode'] == 'local_api_key' else '<YOUR_SUB2API_KEY>'
        lines = [
            'model_provider = "sub2api"',
            f"model = \"{first['model']}\"",
            f"api_url = \"{self.state['deploy']['sub2apiHost'].rstrip('/')}/v1\"",
            f'api_key = "{api_key}"',
            f'requires_openai_auth = {requires}',
        ]
        config_file.write_text('\n'.join(lines) + '\n', encoding='utf-8')

    def _sync_ccswitch(self):
        db_path = self.state['deploy']['ccSwitchDb']
        if not db_path:
            db_path = str(Path.home() / '.cc-switch' / 'cc-switch.db')
        db = Path(db_path)
        if not db.exists():
            return
        conn = sqlite3.connect(str(db), timeout=30)
        try:
            providers = self.state['ccSwitchProviders']
            conn.execute('BEGIN IMMEDIATE')
            for p in providers:
                settings = p.get('settings', {})
                settings_json = json.dumps(settings, ensure_ascii=False, separators=(',', ':'))
                conn.execute(
                    'INSERT INTO providers (id, name, app_type, settings_config) VALUES (?, ?, ?, ?) '
                    'ON CONFLICT(id) DO UPDATE SET name=excluded.name, settings_config=excluded.settings_config',
                    (p['id'], p['name'], p.get('app_type', 'codex'), settings_json)
                )
            conn.commit()
        except Exception:
            conn.rollback()
            raise
        finally:
            conn.close()

    def _write_proxy_script(self):
        p = self.state['proxy']
        if not p.get('enabled'):
            return
        scheme = p.get('type', 'http')
        host = p.get('host', '127.0.0.1')
        port = p.get('port', '7890')
        cred = ''
        if p.get('username'):
            user = p['username']
            passwd = p.get('password', '')
            from urllib.parse import quote
            cred = f"{quote(user)}:{quote(passwd)}@"
        proxy_url = f"{scheme}://{cred}{host}:{port}"
        script_path = self.router_root / 'scripts' / 'Start-Router-WithProxy.ps1'
        lines = [
            "Set-StrictMode -Version Latest",
            "$ErrorActionPreference = 'Stop'",
            f"$env:HTTP_PROXY = '{proxy_url}'",
            f"$env:HTTPS_PROXY = '{proxy_url}'",
            f"$env:ALL_PROXY = '{proxy_url}'",
            f"Write-Host 'Proxy enabled: {proxy_url}'",
            f"& '{self.router_root / 'scripts' / 'Start-Router.ps1'}' @args",
        ]
        script_path.write_text('\n'.join(lines) + '\n', encoding='utf-8')

    def show_main_dashboard(self):
        for w in self.root.winfo_children():
            w.destroy()
        self._load_config_if_exists()
        self.notebook = ttk.Notebook(self.root)
        self.notebook.pack(fill='both', expand=True, padx=12, pady=12)
        self._build_dashboard_tab()
        self._build_models_tab()
        self._build_settings_tab()
        self._build_proxy_tab()
        self._build_log_tab()

    def _load_config_if_exists(self):
        if self.config_path.exists():
            try:
                self.state = json.loads(self.config_path.read_text(encoding='utf-8'))
            except Exception:
                pass

    def _build_dashboard_tab(self):
        tab = ttk.Frame(self.notebook)
        self.notebook.add(tab, text='控制台')
        ttk.Label(tab, text='Codex-Router 控制台', font=('Microsoft YaHei', 16, 'bold')).pack(anchor='w', pady=10)
        status = '已配置' if self.config_path.exists() else '未配置'
        ttk.Label(tab, text=f'状态：{status}', font=('Microsoft YaHei', 11)).pack(anchor='w', pady=6)
        ttk.Label(tab, text=f'项目目录：{self.router_root}').pack(anchor='w', pady=2)

        btn_frame = ttk.Frame(tab)
        btn_frame.pack(fill='x', pady=20)
        ttk.Button(btn_frame, text='启动路由', command=self._start_router).pack(side='left', padx=4)
        ttk.Button(btn_frame, text='停止路由', command=self._stop_router).pack(side='left', padx=4)
        ttk.Button(btn_frame, text='重新运行配置向导', command=self.show_wizard).pack(side='left', padx=4)

        ttk.Label(tab, text='提示：首次启动路由后，Sub2API 会自动拉起。然后即可在 Codex 桌面端选择模型。',
                  wraplength=760, foreground='#64748b').pack(anchor='w', pady=20)

    def _start_router(self):
        script = self.router_root / 'scripts' / 'Start-Router-WithProxy.ps1'
        if not script.exists():
            script = self.router_root / 'scripts' / 'Start-Router.ps1'
        if not script.exists():
            messagebox.showerror('错误', '未找到启动脚本')
            return
        try:
            subprocess.Popen(
                ['powershell', '-ExecutionPolicy', 'Bypass', '-File', str(script)],
                cwd=str(self.router_root), creationflags=subprocess.CREATE_NEW_CONSOLE
            )
            self._log(f'已启动路由：{script}')
        except Exception as e:
            messagebox.showerror('启动失败', str(e))

    def _stop_router(self):
        script = self.router_root / 'scripts' / 'Stop-Router.ps1'
        if not script.exists():
            messagebox.showerror('错误', '未找到停止脚本')
            return
        try:
            subprocess.run(['powershell', '-ExecutionPolicy', 'Bypass', '-File', str(script)], cwd=str(self.router_root), check=True)
            self._log('路由已停止')
        except Exception as e:
            messagebox.showerror('停止失败', str(e))

    def _build_models_tab(self):
        tab = ttk.Frame(self.notebook)
        self.notebook.add(tab, text='模型渠道')
        top = ttk.Frame(tab)
        top.pack(fill='x', pady=8)
        ttk.Button(top, text='+ 添加模型', command=self._add_model_dialog).pack(side='right', padx=4)
        cols = ('model', 'alias', 'baseURL', 'priority')
        self.model_tree = ttk.Treeview(tab, columns=cols, show='headings', height=10)
        for c, t in zip(cols, ['模型名称', '别名', 'Base URL', '优先级']):
            self.model_tree.heading(c, text=t)
        self.model_tree.column('model', width=180)
        self.model_tree.column('alias', width=140)
        self.model_tree.column('baseURL', width=360)
        self.model_tree.column('priority', width=70, anchor='center')
        self.model_tree.pack(fill='both', expand=True, pady=8)
        self._refresh_model_tree()
        btns = ttk.Frame(tab)
        btns.pack(fill='x', pady=8)
        ttk.Button(btns, text='编辑', command=self._edit_model_dialog).pack(side='left', padx=4)
        ttk.Button(btns, text='删除', command=self._delete_model).pack(side='left', padx=4)
        ttk.Button(btns, text='保存并应用', command=self._apply_from_dashboard).pack(side='left', padx=4)

    def _refresh_model_tree(self):
        self.model_tree.delete(*self.model_tree.get_children())
        for m in self.state.get('models', []):
            self.model_tree.insert('', 'end', values=(m['model'], m['alias'], m['baseURL'], m['priority']))

    def _model_dialog(self, model=None):
        dlg = tk.Toplevel(self.root)
        dlg.title('编辑模型')
        dlg.geometry('520x360')
        dlg.transient(self.root)
        dlg.grab_set()
        m = dict(model) if model else default_model_dict()
        vars_map = {}
        fields = [
            ('model', '模型名称'),
            ('alias', '别名'),
            ('baseURL', 'Base URL'),
            ('apiKey', 'API Key'),
            ('priority', '优先级'),
            ('weight', '权重'),
            ('extra', '其它参数 (JSON)'),
        ]
        multimodal_var = tk.StringVar(value=str(m.get('multimodal', 'auto')))
        for i, (k, label) in enumerate(fields):
            ttk.Label(dlg, text=label).grid(row=i, column=0, sticky='w', padx=8, pady=6)
            var = tk.StringVar(value=str(m.get(k, '')))
            show = '*' if k == 'apiKey' else ''
            ttk.Entry(dlg, textvariable=var, width=45, show=show).grid(row=i, column=1, padx=8, pady=6)
            vars_map[k] = var

        mm_row = len(fields)
        ttk.Label(dlg, text='多模态支持').grid(row=mm_row, column=0, sticky='w', padx=8, pady=6)
        mm_combo = ttk.Combobox(dlg, textvariable=multimodal_var, values=['auto', 'true', 'false'], state='readonly', width=15)
        mm_combo.grid(row=mm_row, column=1, sticky='w', padx=8, pady=6)
        ttk.Label(dlg, text='auto=根据模型名自动判断', foreground='#64748b').grid(row=mm_row+1, column=1, sticky='w', padx=8)

        def save():
            try:
                result = {k: v.get() for k, v in vars_map.items()}
                result['priority'] = int(result['priority'])
                result['weight'] = int(result['weight'])
                result['multimodal'] = multimodal_var.get()
                json.loads(result.get('extra') or '{}')
                if not result['model']:
                    raise ValueError('模型名称不能为空')
                if model:
                    idx = self.state['models'].index(model)
                    self.state['models'][idx] = result
                else:
                    self.state['models'].append(result)
                self._refresh_model_tree()
                dlg.destroy()
            except Exception as e:
                messagebox.showerror('错误', str(e))

        btnf = ttk.Frame(dlg)
        btnf.grid(row=len(fields)+2, column=0, columnspan=2, pady=16)
        ttk.Button(btnf, text='保存', command=save).pack(side='left', padx=8)
        ttk.Button(btnf, text='取消', command=dlg.destroy).pack(side='left', padx=8)

    def _add_model_dialog(self):
        self._model_dialog()

    def _edit_model_dialog(self):
        sel = self.model_tree.selection()
        if not sel:
            return
        idx = self.model_tree.index(sel[0])
        self._model_dialog(self.state['models'][idx])

    def _delete_model(self):
        sel = self.model_tree.selection()
        if not sel:
            return
        idx = self.model_tree.index(sel[0])
        if messagebox.askyesno('确认', '删除选中的模型？'):
            del self.state['models'][idx]
            self._refresh_model_tree()

    def _apply_from_dashboard(self):
        try:
            self._build_derived_config()
            self._write_all_files()
            self._write_codex_config()
            if self.state.get('deploy', {}).get('ccSwitchSync'):
                self._sync_ccswitch()
            self._write_proxy_script()
            messagebox.showinfo('保存成功', '配置已更新并应用')
            self._log('配置已更新并应用')
        except Exception as e:
            messagebox.showerror('保存失败', str(e))

    def _build_settings_tab(self):
        tab = ttk.Frame(self.notebook)
        self.notebook.add(tab, text='高级设置')
        ttk.Label(tab, text='登录方式', font=('Microsoft YaHei', 11, 'bold')).pack(anchor='w', pady=(12, 6))
        self.setting_auth_mode = tk.StringVar(value=self.state.get('authMode', 'chatgpt_oauth'))
        ttk.Radiobutton(tab, text='ChatGPT 账号登录', variable=self.setting_auth_mode, value='chatgpt_oauth').pack(anchor='w')
        ttk.Radiobutton(tab, text='本地 API Key 登录', variable=self.setting_auth_mode, value='local_api_key').pack(anchor='w')
        self.setting_local_key = self._entry_var(tab, '本地 API Key', self.state.get('localApiKey', ''), password=True)

        ttk.Label(tab, text='OAuth 兜底', font=('Microsoft YaHei', 11, 'bold')).pack(anchor='w', pady=(18, 6))
        self.setting_oauth_enabled = tk.BooleanVar(value=self.state.get('oauthFallback', {}).get('enabled', True))
        ttk.Checkbutton(tab, text='启用官方 OAuth → 第三方同名模型自动兜底', variable=self.setting_oauth_enabled).pack(anchor='w')

        ttk.Label(tab, text='CC Switch 同步', font=('Microsoft YaHei', 11, 'bold')).pack(anchor='w', pady=(18, 6))
        self.setting_ccsync = tk.BooleanVar(value=self.state.get('deploy', {}).get('ccSwitchSync', False))
        ttk.Checkbutton(tab, text='自动同步 Provider 配置到 CC Switch 数据库', variable=self.setting_ccsync).pack(anchor='w')

        ttk.Label(tab, text='思考档位', font=('Microsoft YaHei', 11, 'bold')).pack(anchor='w', pady=(18, 6))
        self.setting_reasoning_mode = tk.StringVar(value=self.state.get('reasoning', {}).get('mode', 'auto'))
        ttk.Radiobutton(tab, text='自动匹配', variable=self.setting_reasoning_mode, value='auto').pack(anchor='w')
        ttk.Radiobutton(tab, text='手动填写', variable=self.setting_reasoning_mode, value='manual').pack(anchor='w')

        ttk.Button(tab, text='保存设置', command=self._save_settings).pack(anchor='w', pady=20)

    def _save_settings(self):
        self.state['authMode'] = self.setting_auth_mode.get()
        self.state['localApiKey'] = self.setting_local_key.get()
        self.state['oauthFallback']['enabled'] = self.setting_oauth_enabled.get()
        self.state.setdefault('deploy', {})['ccSwitchSync'] = self.setting_ccsync.get()
        self.state['reasoning']['mode'] = self.setting_reasoning_mode.get()
        self.config_path.write_text(json.dumps(self.state, indent=2, ensure_ascii=False), encoding='utf-8')
        messagebox.showinfo('保存成功', '高级设置已保存')

    def _build_proxy_tab(self):
        tab = ttk.Frame(self.notebook)
        self.notebook.add(tab, text='网络代理')
        p = self.state.get('proxy', {})
        self.proxy_enabled_var = tk.BooleanVar(value=p.get('enabled', False))
        ttk.Checkbutton(tab, text='启用代理', variable=self.proxy_enabled_var).pack(anchor='w', pady=8)
        self.proxy_type_var = ttk.Combobox(tab, values=['http', 'https', 'socks5', 'socks5h'], state='readonly', width=15)
        self.proxy_type_var.set(p.get('type', 'http'))
        self.proxy_type_var.pack(anchor='w', pady=4)
        self.proxy_host_var = self._entry_var(tab, '代理地址', p.get('host', '127.0.0.1'))
        self.proxy_port_var = self._entry_var(tab, '代理端口', p.get('port', '7890'))
        self.proxy_user_var = self._entry_var(tab, '用户名（可选）', p.get('username', ''))
        self.proxy_pass_var = self._entry_var(tab, '密码（可选）', p.get('password', ''), password=True)
        ttk.Button(tab, text='保存代理设置', command=self._save_proxy).pack(anchor='w', pady=20)

    def _save_proxy(self):
        self.state['proxy'] = {
            'enabled': self.proxy_enabled_var.get(),
            'type': self.proxy_type_var.get(),
            'host': self.proxy_host_var.get(),
            'port': self.proxy_port_var.get(),
            'username': self.proxy_user_var.get(),
            'password': self.proxy_pass_var.get()
        }
        self.config_path.write_text(json.dumps(self.state, indent=2, ensure_ascii=False), encoding='utf-8')
        self._write_proxy_script()
        messagebox.showinfo('保存成功', '代理设置已保存，并已重新生成启动脚本')

    def _build_log_tab(self):
        tab = ttk.Frame(self.notebook)
        self.notebook.add(tab, text='日志')
        self.log_text = tk.Text(tab, wrap='word', state='disabled', bg='#0f172a', fg='#e2e8f0', font=('Consolas', 10))
        self.log_text.pack(fill='both', expand=True, padx=8, pady=8)

    def _log(self, msg):
        self.log_text.configure(state='normal')
        self.log_text.insert('end', msg + '\n')
        self.log_text.see('end')
        self.log_text.configure(state='disabled')


def default_model_dict():
    return {'model': '', 'alias': '', 'baseURL': '', 'apiKey': '', 'priority': 10, 'weight': 1, 'extra': '{}', 'multimodal': 'auto'}


def main():
    root = tk.Tk()
    app = CodexRouterApp(root)
    root.mainloop()


if __name__ == '__main__':
    main()
