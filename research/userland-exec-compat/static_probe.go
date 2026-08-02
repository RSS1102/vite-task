package main

import (
	"context"
	"fmt"
	"os"
	"os/exec"
	"runtime"
	"sync"
	"time"
)

func main() {
	if len(os.Args) > 1 && os.Args[1] == "--self-child" {
		fmt.Println("go-self-child")
		return
	}

	executable, executableErr := os.Executable()
	data, readErr := os.ReadFile("/etc/hostname")
	var workers sync.WaitGroup
	workers.Add(8)
	for range 8 {
		go func() {
			defer workers.Done()
			for i := 0; i < 1000; i++ {
				_ = i * i
			}
		}()
	}
	workers.Wait()

	childOutput, childErr := exec.Command("/bin/echo", "go-child").CombinedOutput()
	ctx, cancel := context.WithTimeout(context.Background(), 3*time.Second)
	defer cancel()
	selfOutput, selfErr := exec.CommandContext(ctx, executable, "--self-child").CombinedOutput()

	fmt.Printf("go=compat-v1 pid=%d args=%q executable=%q executable_err=%v goroutines=%d hostname_bytes=%d read_err=%v\n",
		os.Getpid(), os.Args, executable, executableErr, runtime.NumGoroutine(), len(data), readErr)
	fmt.Printf("go_child=%q error=%v\n", childOutput, childErr)
	fmt.Printf("go_self_reexec=%q error=%v\n", selfOutput, selfErr)
}
