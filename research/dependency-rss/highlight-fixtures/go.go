// Package pump moves stdin lines through workers to stdout.
package main

import (
	"bufio"
	"flag"
	"fmt"
	"os"
	"strings"
	"sync"
)

var workers = flag.Int("j", 4, "worker goroutines")

func shout(line string) string {
	return strings.ToUpper(strings.TrimSpace(line))
}

func main() {
	flag.Parse()
	in := make(chan string)
	out := make(chan string)
	var wg sync.WaitGroup
	for i := 0; i < *workers; i++ {
		wg.Add(1)
		go func() {
			defer wg.Done()
			for line := range in {
				if line == "" {
					continue
				}
				out <- shout(line)
			}
		}()
	}
	go func() {
		scanner := bufio.NewScanner(os.Stdin)
		for scanner.Scan() {
			in <- scanner.Text()
		}
		close(in)
	}()
	go func() {
		wg.Wait()
		close(out)
	}()
	for line := range out {
		fmt.Println(line)
	}
}
