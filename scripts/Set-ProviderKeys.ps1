Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$routerRoot = Split-Path -Parent $PSScriptRoot
Import-Module "$routerRoot\scripts\CredentialStore.psm1" -Force

Add-Type -AssemblyName System.Windows.Forms
Add-Type -AssemblyName System.Drawing

$form = [Windows.Forms.Form]::new()
$form.Text = 'Codex Router - 安全录入新密钥'
$form.StartPosition = 'CenterScreen'
$form.ClientSize = [Drawing.Size]::new(720, 430)
$form.FormBorderStyle = 'FixedDialog'
$form.MaximizeBox = $false
$form.MinimizeBox = $false
$form.TopMost = $true

$title = [Windows.Forms.Label]::new()
$title.Location = [Drawing.Point]::new(24, 20)
$title.Size = [Drawing.Size]::new(670, 44)
$title.Font = [Drawing.Font]::new('Microsoft YaHei UI', 11, [Drawing.FontStyle]::Bold)
$title.Text = "请粘贴重新生成的密钥。聊天中出现过的旧密钥已泄露，不能继续使用。`r`n密钥只会写入 Windows 凭据管理器。"
$form.Controls.Add($title)

$specs = @(
    @{ Label = '430123 中转站新 Key'; Name = 'RelayApiKey' },
    @{ Label = 'Kimi 主 Key（新）'; Name = 'KimiPrimaryApiKey' },
    @{ Label = 'Kimi 备用 Key（新）'; Name = 'KimiFallbackApiKey' },
    @{ Label = 'OpenRouter 新 Key'; Name = 'OpenRouterApiKey' }
)

$boxes = @{}
$top = 88
foreach ($spec in $specs) {
    $label = [Windows.Forms.Label]::new()
    $label.Location = [Drawing.Point]::new(24, $top + 4)
    $label.Size = [Drawing.Size]::new(190, 28)
    $label.Font = [Drawing.Font]::new('Microsoft YaHei UI', 9)
    $label.Text = $spec.Label
    $form.Controls.Add($label)

    $box = [Windows.Forms.TextBox]::new()
    $box.Location = [Drawing.Point]::new(220, $top)
    $box.Size = [Drawing.Size]::new(470, 28)
    $box.Font = [Drawing.Font]::new('Consolas', 10)
    $box.UseSystemPasswordChar = $true
    $form.Controls.Add($box)
    $boxes[$spec.Name] = $box
    $top += 60
}

$show = [Windows.Forms.CheckBox]::new()
$show.Location = [Drawing.Point]::new(220, 330)
$show.Size = [Drawing.Size]::new(180, 28)
$show.Font = [Drawing.Font]::new('Microsoft YaHei UI', 9)
$show.Text = '临时显示输入内容'
$show.Add_CheckedChanged({
    foreach ($box in $boxes.Values) { $box.UseSystemPasswordChar = -not $show.Checked }
})
$form.Controls.Add($show)

$save = [Windows.Forms.Button]::new()
$save.Location = [Drawing.Point]::new(500, 366)
$save.Size = [Drawing.Size]::new(90, 36)
$save.Font = [Drawing.Font]::new('Microsoft YaHei UI', 9)
$save.Text = '安全保存'
$save.Add_Click({
    try {
        $values = @{}
        foreach ($spec in $specs) {
            $value = $boxes[$spec.Name].Text.Trim()
            if ($value.Length -lt 20) { throw "$($spec.Label) 为空或长度不正确。" }
            if (-not $value.StartsWith('sk-')) { throw "$($spec.Label) 格式不正确。" }
            $values[$spec.Name] = $value
        }
        if ($values.KimiPrimaryApiKey -eq $values.KimiFallbackApiKey) {
            throw 'Kimi 主 Key 和备用 Key 不能相同。'
        }

        foreach ($entry in $values.GetEnumerator()) {
            Set-RouterCredential -Name $entry.Key -Secret $entry.Value
        }
        foreach ($key in @($values.Keys)) { $values[$key] = $null }
        [Windows.Forms.MessageBox]::Show(
            '四枚密钥已安全写入 Windows 凭据管理器。',
            'Codex Router',
            [Windows.Forms.MessageBoxButtons]::OK,
            [Windows.Forms.MessageBoxIcon]::Information
        ) | Out-Null
        $form.DialogResult = [Windows.Forms.DialogResult]::OK
        $form.Close()
    } catch {
        [Windows.Forms.MessageBox]::Show(
            $_.Exception.Message,
            '无法保存',
            [Windows.Forms.MessageBoxButtons]::OK,
            [Windows.Forms.MessageBoxIcon]::Warning
        ) | Out-Null
    }
})
$form.Controls.Add($save)

$cancel = [Windows.Forms.Button]::new()
$cancel.Location = [Drawing.Point]::new(600, 366)
$cancel.Size = [Drawing.Size]::new(90, 36)
$cancel.Font = [Drawing.Font]::new('Microsoft YaHei UI', 9)
$cancel.Text = '稍后再填'
$cancel.DialogResult = [Windows.Forms.DialogResult]::Cancel
$form.Controls.Add($cancel)

$form.AcceptButton = $save
$form.CancelButton = $cancel
$result = $form.ShowDialog()
if ($result -eq [Windows.Forms.DialogResult]::OK) {
    Write-Output 'Provider keys were saved to Windows Credential Manager.'
} else {
    Write-Output 'Provider key entry was cancelled.'
}
