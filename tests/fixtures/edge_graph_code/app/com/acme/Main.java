package com.acme;

import com.acme.util.Strings;
import java.io.File;
import java.util.List;

public class Main {
    public static void main(String[] args) throws Exception {
        // new FileReader("ghost.csv");
        List<String> rows = java.nio.file.Files.readAllLines(java.nio.file.Paths.get("data", "customers.csv"));
        System.out.println(Strings.shout("done"));
    }
}
