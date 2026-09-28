QT += core gui qml quick quickcontrols2 printsupport
CONFIG += c++17 console
CONFIG -= app_bundle
TARGET = shots
TEMPLATE = app

SOURCES += shots.cpp \
    ../../src/notesbackend.cpp \
    ../../src/markdownhighlighter.cpp
HEADERS += ../../src/notesbackend.h ../../src/markdownhighlighter.h
INCLUDEPATH += ../../src
