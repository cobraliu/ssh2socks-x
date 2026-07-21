package main

import (
	"fmt"
	"image/color"
	"os"

	"fyne.io/fyne/v2"
	"fyne.io/fyne/v2/app"
	"fyne.io/fyne/v2/canvas"
	"fyne.io/fyne/v2/container"
	"fyne.io/fyne/v2/dialog"
	"fyne.io/fyne/v2/widget"

	"ssh2socks.local/core"
)

func main() {
	a := app.NewWithID("local.ssh2socks.desktop")
	w := a.NewWindow("ssh2socks — SSH→SOCKS 代理")
	u := &ui{app: a, win: w, prefs: a.Preferences(), labelToAlias: map[string]string{}}
	u.build()
	w.Resize(fyne.NewSize(660, 640))
	w.ShowAndRun()
}

// ui owns every widget plus the live engine. All engine callbacks are marshalled
// onto Fyne's UI goroutine with fyne.Do before touching any widget.
type ui struct {
	app   fyne.App
	win   fyne.Window
	prefs fyne.Preferences

	configPath   *widget.Entry
	importBtn    *widget.Button
	hostSelect   *widget.Select
	labelToAlias map[string]string
	manualHost   *widget.Entry
	manualPort   *widget.Entry
	manualUser   *widget.Entry

	identityPath *widget.Entry
	pickBtn      *widget.Button
	passphrase   *widget.Entry

	listen        *widget.Entry
	probeURL      *widget.Entry
	autoReconnect *widget.Check

	startStop *widget.Button
	status    *canvas.Text
	socksAddr *widget.Label
	copyBtn   *widget.Button
	logBox    *widget.Entry

	engine *core.Engine
}

func (u *ui) build() {
	p := loadProfile(u.prefs)

	u.configPath = &widget.Entry{Text: p.ConfigPath}
	u.importBtn = widget.NewButton("导入…", u.onImport)
	u.hostSelect = widget.NewSelect(nil, func(string) {})
	u.hostSelect.PlaceHolder = "（导入配置后选择）"

	u.manualHost = &widget.Entry{Text: p.Host, PlaceHolder: "example.com"}
	u.manualPort = &widget.Entry{Text: p.Port, PlaceHolder: "22"}
	u.manualUser = &widget.Entry{Text: p.User, PlaceHolder: "登录用户名"}

	u.identityPath = &widget.Entry{Text: p.IdentityPath, PlaceHolder: "~/.ssh/id_ed25519"}
	u.pickBtn = widget.NewButton("选择…", u.onPickKey)
	u.passphrase = widget.NewPasswordEntry()
	u.passphrase.SetPlaceHolder("私钥口令（不保存）")

	u.listen = &widget.Entry{Text: p.Listen}
	u.probeURL = &widget.Entry{Text: p.ProbeURL, PlaceHolder: "http://www.gstatic.com/generate_204"}
	u.autoReconnect = widget.NewCheck("自动重连", nil)
	u.autoReconnect.Checked = p.AutoReconnect

	u.startStop = widget.NewButton("启动", u.onStartStop)
	u.startStop.Importance = widget.HighImportance
	u.status = canvas.NewText("", color.Gray{Y: 0x88})
	u.status.TextStyle = fyne.TextStyle{Bold: true}
	u.socksAddr = widget.NewLabel("SOCKS5: —")
	u.copyBtn = widget.NewButton("复制", u.onCopy)
	u.logBox = widget.NewMultiLineEntry()
	u.logBox.Wrapping = fyne.TextWrapWord

	form := widget.NewForm(
		widget.NewFormItem("SSH 配置", container.NewBorder(nil, nil, nil, u.importBtn, u.configPath)),
		widget.NewFormItem("主机(配置)", u.hostSelect),
		widget.NewFormItem("主机(手动)", u.manualHost),
		widget.NewFormItem("端口", u.manualPort),
		widget.NewFormItem("用户", u.manualUser),
		widget.NewFormItem("私钥文件", container.NewBorder(nil, nil, nil, u.pickBtn, u.identityPath)),
		widget.NewFormItem("口令", u.passphrase),
		widget.NewFormItem("本地监听", u.listen),
		widget.NewFormItem("探测 URL", u.probeURL),
		widget.NewFormItem("", u.autoReconnect),
	)

	statusRow := container.NewBorder(nil, nil, u.status, u.copyBtn, u.socksAddr)
	title := canvas.NewText("ssh2socks", color.NRGBA{R: 0x2e, G: 0xa0, B: 0x43, A: 0xff})
	title.TextSize = 20
	title.TextStyle = fyne.TextStyle{Bold: true}

	top := container.NewVBox(title, form, u.startStop, statusRow, widget.NewLabel("日志"))
	u.win.SetContent(container.NewBorder(top, nil, nil, nil, container.NewScroll(u.logBox)))

	u.applyState(core.StateStopped, "")

	// Re-load a previously imported config so the dropdown is populated on launch.
	if p.ConfigPath != "" {
		if b, err := os.ReadFile(p.ConfigPath); err == nil {
			u.populateHosts(string(b), p.ConfigPath, p.Alias)
		}
	}
}

func (u *ui) onImport() {
	dialog.ShowFileOpen(func(rc fyne.URIReadCloser, err error) {
		if err != nil || rc == nil {
			return
		}
		defer rc.Close()
		path := rc.URI().Path()
		b, err := os.ReadFile(path)
		if err != nil {
			dialog.ShowError(fmt.Errorf("读取配置失败: %w", err), u.win)
			return
		}
		u.populateHosts(string(b), path, "")
	}, u.win)
}

// populateHosts parses an OpenSSH config through core and fills the host dropdown,
// re-selecting `selectAlias` if it is still present.
func (u *ui) populateHosts(configText, path, selectAlias string) {
	hosts, err := core.ListHosts(configText)
	if err != nil {
		dialog.ShowError(fmt.Errorf("解析配置失败: %w", err), u.win)
		return
	}
	opts := make([]string, 0, len(hosts))
	u.labelToAlias = make(map[string]string, len(hosts))
	var reselect string
	for _, h := range hosts {
		l := hostLabel(h)
		opts = append(opts, l)
		u.labelToAlias[l] = h.Alias
		if h.Alias == selectAlias {
			reselect = l
		}
	}
	u.hostSelect.Options = opts
	u.hostSelect.Refresh()
	u.configPath.SetText(path)
	if reselect != "" {
		u.hostSelect.SetSelected(reselect)
	}
	u.appendLog(fmt.Sprintf("已导入配置：%d 个主机（%s）", len(hosts), path))
}

func (u *ui) onPickKey() {
	dialog.ShowFileOpen(func(rc fyne.URIReadCloser, err error) {
		if err != nil || rc == nil {
			return
		}
		defer rc.Close()
		u.identityPath.SetText(rc.URI().Path())
	}, u.win)
}

func (u *ui) onCopy() {
	if addr := u.socksLive(); addr != "" {
		u.app.Clipboard().SetContent(addr)
	}
}

func (u *ui) onStartStop() {
	if u.engine != nil {
		u.onStop()
		return
	}
	u.onStart()
}

func (u *ui) onStart() {
	p := u.collectForm()
	saveProfile(u.prefs, p)

	var configText string
	if p.ConfigPath != "" {
		b, err := os.ReadFile(p.ConfigPath)
		if err != nil {
			dialog.ShowError(fmt.Errorf("读取配置失败: %w", err), u.win)
			return
		}
		configText = string(b)
	}
	if p.IdentityPath == "" {
		dialog.ShowError(fmt.Errorf("请选择私钥文件"), u.win)
		return
	}
	keyPEM, err := os.ReadFile(p.IdentityPath)
	if err != nil {
		dialog.ShowError(fmt.Errorf("读取私钥失败: %w", err), u.win)
		return
	}
	cfg, err := formToConfig(p, configText, string(keyPEM), u.passphrase.Text)
	if err != nil {
		dialog.ShowError(err, u.win)
		return
	}

	ev := core.Events{
		OnState: func(s core.State, msg string) { fyne.Do(func() { u.applyState(s, msg) }) },
		OnLog:   func(line string) { fyne.Do(func() { u.appendLog(line) }) },
		OnProbe: func(ok bool, ms int64, msg string) {
			fyne.Do(func() {
				res := "失败"
				if ok {
					res = "正常"
				}
				u.appendLog(fmt.Sprintf("连通性探测: %s (%dms) %s", res, ms, msg))
			})
		},
	}
	u.engine = core.NewEngine(cfg, ev)
	if err := u.engine.Start(); err != nil {
		u.engine = nil
		dialog.ShowError(err, u.win)
		return
	}
	u.appendLog("启动中…")
	u.setRunning(true)
}

func (u *ui) onStop() {
	e := u.engine
	if e == nil {
		return
	}
	u.startStop.Disable()
	u.appendLog("正在停止…")
	go func() {
		e.Stop() // blocks until goroutines drain
		fyne.Do(func() {
			u.engine = nil
			u.startStop.Enable()
			u.socksAddr.SetText("SOCKS5: —")
			u.setRunning(false)
		})
	}()
}

func (u *ui) collectForm() profile {
	return profile{
		ConfigPath:    u.configPath.Text,
		Alias:         u.labelToAlias[u.hostSelect.Selected],
		Host:          u.manualHost.Text,
		Port:          u.manualPort.Text,
		User:          u.manualUser.Text,
		IdentityPath:  u.identityPath.Text,
		Listen:        u.listen.Text,
		ProbeURL:      u.probeURL.Text,
		AutoReconnect: u.autoReconnect.Checked,
	}
}

func (u *ui) socksLive() string {
	if u.engine != nil {
		return u.engine.SocksAddr()
	}
	return ""
}

func (u *ui) applyState(s core.State, msg string) {
	var c color.Color
	var label string
	switch s {
	case core.StateConnected:
		c, label = color.NRGBA{R: 0x2e, G: 0xa0, B: 0x43, A: 0xff}, "已连接"
		if a := u.socksLive(); a != "" {
			u.socksAddr.SetText("SOCKS5: " + a)
		}
	case core.StateConnecting:
		c, label = color.NRGBA{R: 0xd2, G: 0x8e, B: 0x00, A: 0xff}, "连接中"
	case core.StateError:
		c, label = color.NRGBA{R: 0xd0, G: 0x33, B: 0x2f, A: 0xff}, "错误"
	default:
		c, label = color.Gray{Y: 0x88}, "已停止"
	}
	if msg != "" && s != core.StateConnected {
		label += " — " + msg
	}
	u.status.Text = "● " + label
	u.status.Color = c
	u.status.Refresh()
}

func (u *ui) appendLog(line string) {
	txt := u.logBox.Text
	if txt != "" {
		txt += "\n"
	}
	txt += line
	if len(txt) > 20000 { // soft cap so a long session doesn't grow unbounded
		txt = txt[len(txt)-20000:]
	}
	u.logBox.SetText(txt)
	u.logBox.CursorRow = len(u.logBox.Text)
}

func (u *ui) setRunning(running bool) {
	if running {
		u.startStop.SetText("停止")
		u.startStop.Importance = widget.DangerImportance
	} else {
		u.startStop.SetText("启动")
		u.startStop.Importance = widget.HighImportance
	}
	u.startStop.Refresh()
	inputs := []fyne.Disableable{
		u.configPath, u.importBtn, u.hostSelect, u.manualHost, u.manualPort, u.manualUser,
		u.identityPath, u.pickBtn, u.passphrase, u.listen, u.probeURL, u.autoReconnect,
	}
	for _, in := range inputs {
		if running {
			in.Disable()
		} else {
			in.Enable()
		}
	}
}
